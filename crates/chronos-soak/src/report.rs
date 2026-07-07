//! Turning the raw time series and server log into headline figures, a verdict,
//! and a self-contained Markdown report suitable for a CI step summary and
//! artifact.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

use std::fmt::Write as _;
use std::time::Duration;

use crate::config::Config;
use crate::monitor::Sample;
use crate::server::GeoStatus;
use crate::wsprobe::WsProbe;

const BYTES_PER_GIB: f64 = 1_073_741_824.0;
const SECS_PER_DAY: f64 = 86_400.0;

/// Headline figures derived from the sample time series.
#[derive(Debug, Clone)]
pub struct Aggregate {
    pub samples: usize,
    pub window_s: u64,
    pub cpu_core_pct: f64,
    pub cpu_host_pct: f64,
    pub rss_avg_mib: f64,
    pub rss_peak_mib: f64,
    pub ingress_kib_s: f64,
    pub ingress_gib_day: f64,
    pub ingress_reconnected: bool,
    pub ingress_measured: bool,
    pub msgs_total: u64,
    pub msgs_per_s: f64,
    pub bytes_per_msg: Option<f64>,
    pub dropped: u64,
    pub drop_ratio_pct: f64,
    pub anomalies: u64,
    pub hijack: u64,
    pub leak: u64,
    pub churn: u64,
    pub nodes: u64,
    pub edges: u64,
    pub deltas: u64,
    pub clients_peak: u64,
}

impl Aggregate {
    /// Fold the samples into headline figures. `nproc` normalizes CPU to the host.
    pub fn from_samples(samples: &[Sample], clk_tck: u64, nproc: u64) -> Self {
        let first = samples.first();
        let last = samples.last();
        let (Some(first), Some(last)) = (first, last) else {
            return Self::empty();
        };

        let window_s = last.elapsed_s.saturating_sub(first.elapsed_s).max(1);
        let window = window_s as f64;
        let clk = clk_tck.max(1) as f64;

        let cpu_secs = last.cpu_ticks.saturating_sub(first.cpu_ticks) as f64 / clk;
        let cpu_core_pct = cpu_secs / window * 100.0;

        let rss_sum: u64 = samples.iter().map(|s| s.rss_kb).sum();
        let rss_avg_mib = (rss_sum as f64 / samples.len() as f64) / 1024.0;
        let rss_peak_mib = samples.iter().map(|s| s.rss_kb).max().unwrap_or(0) as f64 / 1024.0;

        // Ingress from the socket byte counter. A mid-window RIS reconnect resets
        // the counter, making the delta negative; fall back to the post-reconnect
        // total and flag the figure as approximate.
        let (ingress_measured, ingress_bytes, reconnected) = match (first.sock_rx, last.sock_rx) {
            (Some(f), Some(l)) if l >= f => (true, (l - f) as f64, false),
            (Some(_), Some(l)) => (true, l as f64, true),
            _ => (false, 0.0, false),
        };
        let ingress_bps = ingress_bytes / window;

        let msgs_total = last.metrics.messages.saturating_sub(first.metrics.messages);
        let msgs_per_s = msgs_total as f64 / window;
        let bytes_per_msg =
            (msgs_total > 0 && ingress_measured).then(|| ingress_bytes / msgs_total as f64);

        let dropped = last.metrics.dropped;
        let denom = msgs_total + dropped;
        let drop_ratio_pct = if denom > 0 {
            dropped as f64 / denom as f64 * 100.0
        } else {
            0.0
        };

        let clients_peak = samples.iter().map(|s| s.metrics.clients).max().unwrap_or(0);

        Self {
            samples: samples.len(),
            window_s,
            cpu_core_pct,
            cpu_host_pct: cpu_core_pct / nproc.max(1) as f64,
            rss_avg_mib,
            rss_peak_mib,
            ingress_kib_s: ingress_bps / 1024.0,
            ingress_gib_day: ingress_bps * SECS_PER_DAY / BYTES_PER_GIB,
            ingress_reconnected: reconnected,
            ingress_measured,
            msgs_total,
            msgs_per_s,
            bytes_per_msg,
            dropped,
            drop_ratio_pct,
            anomalies: last.metrics.anomalies(),
            hijack: last.metrics.hijack,
            leak: last.metrics.leak,
            churn: last.metrics.churn,
            nodes: last.metrics.nodes,
            edges: last.metrics.edges,
            deltas: last.metrics.deltas,
            clients_peak,
        }
    }

    fn empty() -> Self {
        Self {
            samples: 0,
            window_s: 0,
            cpu_core_pct: 0.0,
            cpu_host_pct: 0.0,
            rss_avg_mib: 0.0,
            rss_peak_mib: 0.0,
            ingress_kib_s: 0.0,
            ingress_gib_day: 0.0,
            ingress_reconnected: false,
            ingress_measured: false,
            msgs_total: 0,
            msgs_per_s: 0.0,
            bytes_per_msg: None,
            dropped: 0,
            drop_ratio_pct: 0.0,
            anomalies: 0,
            hijack: 0,
            leak: 0,
            churn: 0,
            nodes: 0,
            edges: 0,
            deltas: 0,
            clients_peak: 0,
        }
    }
}

/// Summary of the server's own log stream.
#[derive(Debug, Clone)]
pub struct LogAnalysis {
    pub info: u64,
    pub warn: u64,
    pub error: u64,
    pub panics: u64,
    pub reconnects: u64,
    pub caida_loaded: bool,
    /// First few WARN/ERROR lines, verbatim, for quick triage.
    pub first_problems: Vec<String>,
    /// First lines of the log (startup wiring), for context.
    pub head: Vec<String>,
}

impl LogAnalysis {
    /// Analyze the captured server log.
    pub fn from_log(log: &str) -> Self {
        // The server may emit ANSI color codes; strip them so level tokens parse
        // and the embedded log is readable regardless of the server's TTY guess.
        let clean: String = strip_ansi(log);
        let log = clean.as_str();
        let mut a = LogAnalysis {
            info: 0,
            warn: 0,
            error: 0,
            panics: 0,
            reconnects: 0,
            caida_loaded: false,
            first_problems: Vec::new(),
            head: log.lines().take(25).map(str::to_owned).collect(),
        };
        for line in log.lines() {
            // tracing's fmt layer emits the level as the second whitespace token.
            match line.split_whitespace().nth(1) {
                Some("INFO") => a.info += 1,
                Some("WARN") => {
                    a.warn += 1;
                    if a.first_problems.len() < 20 {
                        a.first_problems.push(line.to_owned());
                    }
                }
                Some("ERROR") => {
                    a.error += 1;
                    if a.first_problems.len() < 20 {
                        a.first_problems.push(line.to_owned());
                    }
                }
                _ => {}
            }
            if line.contains("panicked at") {
                a.panics += 1;
            }
            if line.contains("ingest: subscribed to UPDATE stream") {
                a.reconnects += 1;
            }
            if line.contains("loaded CAIDA AS relationships") {
                a.caida_loaded = true;
            }
        }
        a
    }

    /// Relationship provider description for the report.
    pub fn relationship_provider(&self) -> &'static str {
        if self.caida_loaded {
            "CAIDA AS-relationship dataset"
        } else {
            "degree-based heuristic (CAIDA unavailable)"
        }
    }
}

/// Remove ANSI CSI escape sequences (for example color codes) from a string.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip an escape sequence: ESC [ ... <final byte in @-~ range>.
            if chars.next() == Some('[') {
                for e in chars.by_ref() {
                    if ('@'..='~').contains(&e) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Overall run verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Warn,
    Fail,
}

impl Verdict {
    fn icon(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Warn => "WARN",
            Verdict::Fail => "FAIL",
        }
    }

    /// Process exit code: only a genuine crash/panic is a hard failure.
    pub fn exit_code(self) -> i32 {
        match self {
            Verdict::Fail => 2,
            _ => 0,
        }
    }
}

/// Decide the verdict and its supporting notes.
///
/// A dead upstream feed is a WARN, not a FAIL: RIS is external and can be
/// transiently unavailable, matching the repo's non-blocking live-data policy.
/// Only a server crash or panic fails the run.
pub fn decide_verdict(
    agg: &Aggregate,
    log: &LogAnalysis,
    server_died: bool,
) -> (Verdict, Vec<String>) {
    let mut verdict = Verdict::Pass;
    let mut notes = Vec::new();
    let escalate = |to: Verdict, note: &str, v: &mut Verdict, n: &mut Vec<String>| {
        if to == Verdict::Fail || *v == Verdict::Pass {
            *v = to;
        }
        n.push(note.to_string());
    };

    if server_died {
        escalate(
            Verdict::Fail,
            "Server process did not survive the window (crash).",
            &mut verdict,
            &mut notes,
        );
    }
    if log.panics > 0 {
        escalate(
            Verdict::Fail,
            &format!("Detected {} panic(s) in the log.", log.panics),
            &mut verdict,
            &mut notes,
        );
    }
    if agg.msgs_total == 0 && verdict != Verdict::Fail {
        escalate(
            Verdict::Warn,
            "Zero RIS messages processed (upstream feed unreachable or empty).",
            &mut verdict,
            &mut notes,
        );
    }
    if log.error > 0 && verdict == Verdict::Pass {
        escalate(
            Verdict::Warn,
            &format!("{} ERROR-level log line(s); review the log.", log.error),
            &mut verdict,
            &mut notes,
        );
    }
    if agg.drop_ratio_pct > 1.0 {
        escalate(
            Verdict::Warn,
            &format!(
                "Ingest drop ratio {:.3}% exceeds 1% (backpressure); review sizing.",
                agg.drop_ratio_pct
            ),
            &mut verdict,
            &mut notes,
        );
    }
    if notes.is_empty() {
        notes.push("No anomalies in behavior; all checks nominal.".to_string());
    }
    (verdict, notes)
}

/// Context needed to render the report beyond the aggregate and log analysis.
pub struct ReportContext<'a> {
    pub config: &'a Config,
    pub geo: &'a GeoStatus,
    pub ws: &'a WsProbe,
    pub nproc: u64,
    pub clk_tck: u64,
    pub verdict: Verdict,
    pub notes: &'a [String],
}

/// Render the full Markdown report.
pub fn render(
    agg: &Aggregate,
    log: &LogAnalysis,
    samples: &[Sample],
    ctx: &ReportContext<'_>,
) -> String {
    let mut out = String::with_capacity(4096);
    let ris = match &ctx.config.ris_host {
        Some(h) => format!("collector filter: {h}"),
        None => "unfiltered firehose".to_string(),
    };
    let ingress_note = if !agg.ingress_measured {
        "  [unmeasured: ss unavailable]"
    } else if agg.ingress_reconnected {
        "  [socket reconnected: approximate]"
    } else {
        ""
    };
    let bytes_per_msg = agg
        .bytes_per_msg
        .map(|v| format!("{v:.0}"))
        .unwrap_or_else(|| "n/a".to_string());

    let _ = writeln!(out, "# Chronos production soak report\n");
    let _ = writeln!(out, "## Verdict: {}\n", ctx.verdict.icon());
    for note in ctx.notes {
        let _ = writeln!(out, "- {note}");
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "## Run\n");
    let _ = writeln!(out, "| Field | Value |");
    let _ = writeln!(out, "|---|---|");
    let _ = writeln!(
        out,
        "| Window | {}s measured ({}s warmup, {}s sampling, {} samples) |",
        agg.window_s,
        ctx.config.warmup.as_secs(),
        ctx.config.interval.as_secs(),
        agg.samples
    );
    let _ = writeln!(
        out,
        "| Host | {} vCPU, CLK_TCK={} |",
        ctx.nproc, ctx.clk_tck
    );
    let _ = writeln!(out, "| RIS feed | {ris} |");
    let _ = writeln!(out, "| Geo | {} |", ctx.geo.describe());
    let _ = writeln!(out, "| Relationships | {} |", log.relationship_provider());
    let _ = writeln!(out, "| RIS (re)connections | {} |\n", log.reconnects);

    let _ = writeln!(out, "## Resource usage\n");
    let _ = writeln!(out, "| Resource | Value |");
    let _ = writeln!(out, "|---|---|");
    let _ = writeln!(
        out,
        "| CPU | {:.1}% of one core ({:.1}% of the {}-vCPU host) |",
        agg.cpu_core_pct, agg.cpu_host_pct, ctx.nproc
    );
    let _ = writeln!(
        out,
        "| Memory RSS | {:.1} MiB avg, {:.1} MiB peak |",
        agg.rss_avg_mib, agg.rss_peak_mib
    );
    let _ = writeln!(
        out,
        "| Ingress | {:.1} KiB/s (~{:.2} GiB/day){} |\n",
        agg.ingress_kib_s, agg.ingress_gib_day, ingress_note
    );

    let _ = writeln!(out, "## Performance\n");
    let _ = writeln!(out, "| Metric | Value |");
    let _ = writeln!(out, "|---|---|");
    let _ = writeln!(
        out,
        "| Throughput | {:.0} RIS msg/s ({} in window) |",
        agg.msgs_per_s, agg.msgs_total
    );
    let _ = writeln!(
        out,
        "| Per message | {bytes_per_msg} bytes ingress/message |"
    );
    let _ = writeln!(
        out,
        "| Ingest dropped | {} frames ({:.3}% of received) |",
        agg.dropped, agg.drop_ratio_pct
    );
    let _ = writeln!(
        out,
        "| Topology | {} ASNs, {} edges at window end |",
        agg.nodes, agg.edges
    );
    let _ = writeln!(
        out,
        "| Anomalies | {} total (hijack {}, leak {}, churn {}) |",
        agg.anomalies, agg.hijack, agg.leak, agg.churn
    );
    let _ = writeln!(
        out,
        "| Deltas broadcast | {} (peak {} WS client(s)) |",
        agg.deltas, agg.clients_peak
    );
    let _ = writeln!(out, "| WS egress probe | {} |\n", ctx.ws.summary());

    render_time_series(&mut out, samples);

    let _ = writeln!(out, "## Console log\n");
    let _ = writeln!(out, "| Level | Count |");
    let _ = writeln!(out, "|---|--:|");
    let _ = writeln!(out, "| INFO | {} |", log.info);
    let _ = writeln!(out, "| WARN | {} |", log.warn);
    let _ = writeln!(out, "| ERROR | {} |", log.error);
    let _ = writeln!(out, "| panic | {} |\n", log.panics);

    if !log.first_problems.is_empty() {
        let _ = writeln!(out, "First WARN/ERROR lines:\n");
        let _ = writeln!(out, "```text");
        for line in &log.first_problems {
            let _ = writeln!(out, "{line}");
        }
        let _ = writeln!(out, "```\n");
    }

    let _ = writeln!(
        out,
        "<details><summary>Startup log (first 25 lines)</summary>\n"
    );
    let _ = writeln!(out, "```text");
    for line in &log.head {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(out, "```\n");
    let _ = writeln!(out, "</details>");

    out
}

/// Render a downsampled time-series table (about a dozen rows) so memory growth
/// and throughput ramp are visible at a glance.
fn render_time_series(out: &mut String, samples: &[Sample]) {
    if samples.is_empty() {
        return;
    }
    let _ = writeln!(out, "## Time series\n");
    let _ = writeln!(
        out,
        "| t (s) | RSS MiB | msgs | Δmsg/s | ASNs | edges | dropped | clients |"
    );
    let _ = writeln!(out, "|--:|--:|--:|--:|--:|--:|--:|--:|");

    let step = (samples.len() / 12).max(1);
    let mut prev: Option<&Sample> = None;
    let mut i = 0;
    while i < samples.len() {
        let s = &samples[i];
        let rate = match prev {
            Some(p) => {
                let dt = s.elapsed_s.saturating_sub(p.elapsed_s).max(1);
                s.metrics.messages.saturating_sub(p.metrics.messages) as f64 / dt as f64
            }
            None => 0.0,
        };
        let _ = writeln!(
            out,
            "| {} | {:.1} | {} | {:.0} | {} | {} | {} | {} |",
            s.elapsed_s,
            s.rss_kb as f64 / 1024.0,
            s.metrics.messages,
            rate,
            s.metrics.nodes,
            s.metrics.edges,
            s.metrics.dropped,
            s.metrics.clients,
        );
        prev = Some(s);
        i += step;
    }
    let _ = writeln!(out);
}

/// A compact one-line console summary, echoing the old baseline output.
pub fn console_summary(agg: &Aggregate, verdict: Verdict, window: Duration) -> String {
    format!(
        "[{}] window {}s: CPU {:.1}%/core, RSS {:.0}/{:.0} MiB avg/peak, \
         ingress {:.0} KiB/s, {:.0} msg/s, dropped {} ({:.2}%), anomalies {}",
        verdict.icon(),
        window.as_secs(),
        agg.cpu_core_pct,
        agg.rss_avg_mib,
        agg.rss_peak_mib,
        agg.ingress_kib_s,
        agg.msgs_per_s,
        agg.dropped,
        agg.drop_ratio_pct,
        agg.anomalies,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::MetricsSnapshot;

    fn sample(elapsed: u64, rss: u64, ticks: u64, rx: u64, msgs: u64) -> Sample {
        Sample {
            elapsed_s: elapsed,
            rss_kb: rss,
            cpu_ticks: ticks,
            sock_rx: Some(rx),
            metrics: MetricsSnapshot {
                messages: msgs,
                ..MetricsSnapshot::default()
            },
        }
    }

    #[test]
    fn aggregate_computes_rates_and_peaks() {
        let samples = vec![
            sample(0, 100_000, 0, 0, 0),
            sample(10, 120_000, 500, 1_000_000, 40_000),
            sample(20, 110_000, 1000, 2_000_000, 80_000),
        ];
        let agg = Aggregate::from_samples(&samples, 100, 4);
        assert_eq!(agg.samples, 3);
        assert_eq!(agg.window_s, 20);
        assert_eq!(agg.msgs_total, 80_000);
        assert_eq!(agg.msgs_per_s as u64, 4000);
        // 1000 ticks / 100 Hz = 10 CPU-seconds over 20s = 50% of a core.
        assert_eq!(agg.cpu_core_pct as u64, 50);
        assert_eq!(agg.cpu_host_pct as u64, 12); // 50 / 4
        assert_eq!(agg.rss_peak_mib as u64, 120_000 / 1024);
    }

    #[test]
    fn reconnect_makes_ingress_approximate() {
        let samples = vec![
            sample(0, 100_000, 0, 5_000_000, 0),
            sample(10, 100_000, 100, 1_000, 1000), // counter reset (dropped below start)
        ];
        let agg = Aggregate::from_samples(&samples, 100, 1);
        assert!(agg.ingress_reconnected);
        assert!(agg.ingress_measured);
    }

    #[test]
    fn drop_ratio_and_verdict_warn_on_backpressure() {
        let mut samples = vec![
            sample(0, 100_000, 0, 0, 0),
            sample(10, 100_000, 10, 100, 100),
        ];
        samples[1].metrics.dropped = 50;
        let agg = Aggregate::from_samples(&samples, 100, 1);
        assert!(agg.drop_ratio_pct > 1.0);
        let log = LogAnalysis::from_log("");
        let (verdict, _) = decide_verdict(&agg, &log, false);
        assert_eq!(verdict, Verdict::Warn);
    }

    #[test]
    fn crash_is_fail() {
        let agg =
            Aggregate::from_samples(&[sample(0, 1, 0, 0, 1), sample(10, 1, 1, 1, 10)], 100, 1);
        let log = LogAnalysis::from_log("");
        let (verdict, _) = decide_verdict(&agg, &log, true);
        assert_eq!(verdict, Verdict::Fail);
        assert_eq!(verdict.exit_code(), 2);
    }

    #[test]
    fn log_analysis_counts_levels_and_panics() {
        let log = "\
2026-01-01T00:00:00Z  INFO chronos: starting
2026-01-01T00:00:01Z  INFO ingest: subscribed to UPDATE stream
2026-01-01T00:00:02Z  WARN chronos: geo disabled
2026-01-01T00:00:03Z  ERROR something bad
thread 'main' panicked at src/x.rs:1:1";
        let a = LogAnalysis::from_log(log);
        assert_eq!(a.info, 2);
        assert_eq!(a.warn, 1);
        assert_eq!(a.error, 1);
        assert_eq!(a.panics, 1);
        assert_eq!(a.reconnects, 1);
        assert_eq!(a.first_problems.len(), 2);
    }

    #[test]
    fn strips_ansi_before_counting_levels() {
        // tracing's colored output wraps the level token in CSI escapes.
        let log = "2026-01-01T00:00:00Z \u{1b}[32m INFO\u{1b}[0m chronos: starting";
        let a = LogAnalysis::from_log(log);
        assert_eq!(a.info, 1);
        assert!(a.head[0].contains("INFO"));
        assert!(!a.head[0].contains('\u{1b}'));
    }
}
