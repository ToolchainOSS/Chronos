//! Per-process kernel accounting, read directly from `/proc` and `ss`.
//!
//! These are the same sources the previous shell harness used, but parsed with
//! typed helpers and explicit error handling instead of chained `awk`:
//! - RSS from `/proc/<pid>/status` (`VmRSS`).
//! - CPU time from `/proc/<pid>/stat` (`utime + stime`, in clock ticks).
//! - RIS wire ingress from the TCP socket's `bytes_received`, read via `ss`
//!   because `/proc/<pid>/io` `rchar` does NOT count socket `recv()` traffic.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

use std::fs;
use std::process::Command;

/// Clock ticks per second (`CLK_TCK`), used to convert CPU ticks to seconds.
/// Linux fixes this at 100 on all mainstream configurations; we read it from
/// `getconf` and fall back to 100 if that is unavailable.
pub fn clk_tck() -> u64 {
    Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(100)
}

/// Resident set size in kibibytes from `/proc/<pid>/status`, or `None` if the
/// process is gone or the field is absent.
pub fn rss_kb(pid: u32) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// Cumulative CPU time (`utime + stime`) in clock ticks from `/proc/<pid>/stat`.
///
/// The second field (`comm`) may contain spaces and parentheses, so parsing
/// starts after the final `)`; `utime` and `stime` are then fields 14 and 15 of
/// the original line, i.e. indices 12 and 13 (0-based) of the remainder.
pub fn cpu_ticks(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    // After the ')' the next field is `state`; utime/stime are fields 14/15 of
    // the full line, which are indices 11/12 here (state is index 0).
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// Cumulative bytes received on the process's established TCP sockets whose peer
/// port is 80 (the RIS Live WebSocket). Summed across connections.
///
/// Returns `None` when `ss` is unavailable or produced nothing parseable, so
/// the caller can flag ingress as unmeasured rather than reporting a bogus zero.
pub fn ris_socket_rx_bytes(pid: u32) -> Option<u64> {
    let output = Command::new("ss")
        .args(["-tinp", "state", "established", "( dport = :80 )"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let tag = format!("pid={pid},");
    let mut total: u64 = 0;
    let mut found = false;
    let mut in_ours = false;
    for line in text.lines() {
        if line.contains(&tag) {
            // The socket header line names the owning process; the following
            // metrics line carries bytes_received for that same socket.
            in_ours = true;
            continue;
        }
        if in_ours {
            if let Some(bytes) = extract_bytes_received(line) {
                total += bytes;
                found = true;
            }
            in_ours = false;
        }
    }
    found.then_some(total)
}

/// Parse `bytes_received:<n>` out of an `ss` metrics line.
fn extract_bytes_received(line: &str) -> Option<u64> {
    let start = line.find("bytes_received:")? + "bytes_received:".len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bytes_received_field() {
        let line = "\t bytes_acked:10 bytes_received:123456 segs_out:5";
        assert_eq!(extract_bytes_received(line), Some(123_456));
    }

    #[test]
    fn missing_bytes_received_is_none() {
        assert_eq!(extract_bytes_received("\t cwnd:10 rtt:1.2/0.3"), None);
    }

    #[test]
    fn reads_own_rss() {
        // Our own process always has a VmRSS line on Linux.
        let pid = std::process::id();
        assert!(rss_kb(pid).is_some_and(|kb| kb > 0));
    }

    #[test]
    fn reads_own_cpu_ticks() {
        let pid = std::process::id();
        assert!(cpu_ticks(pid).is_some());
    }
}
