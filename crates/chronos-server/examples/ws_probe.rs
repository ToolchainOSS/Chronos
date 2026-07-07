//! Minimal WebSocket egress probe used by the soak harness
//! ([scripts/soak-test.sh](../../../scripts/soak-test.sh)) to prove the full
//! ingest -> detect -> broadcast -> egress path works end to end against real
//! data, and to make the `chronos_deltas_broadcast_total` /
//! `chronos_connected_clients` metrics meaningful (the broadcast is skipped when
//! no client is subscribed).
//!
//! It connects to a running server's `/ws`, drains delta frames for a bounded
//! window, and prints one machine-readable summary line the harness parses:
//!
//! ```text
//! ws_probe: connected=true frames=1234 bytes=56789 link_up=1200 link_down=30 area_degraded=4 other=0
//! ```
//!
//! A connect failure prints `connected=false` and exits 0: egress is a bonus
//! signal, not a hard gate, so a probe hiccup never fails the soak.
//!
//! Usage: `ws_probe [ws_url] [seconds]`; env `CHRONOS_WS_URL` and
//! `WS_PROBE_SECS` override, in that order of precedence (args win).
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

use std::time::Duration;

use futures_util::StreamExt;
use tokio::time::{Instant, sleep};
use tokio_tungstenite::tungstenite::Message;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .or_else(|| std::env::var("CHRONOS_WS_URL").ok())
        .unwrap_or_else(|| "ws://127.0.0.1:8080/ws".to_string());
    let secs = args
        .next()
        .or_else(|| std::env::var("WS_PROBE_SECS").ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);

    let socket = match tokio_tungstenite::connect_async(&url).await {
        Ok((socket, _resp)) => socket,
        Err(err) => {
            // Egress is a bonus check; a probe failure must not fail the soak.
            println!("ws_probe: connected=false error={err}");
            return;
        }
    };

    let (_sink, mut stream) = socket.split();
    let deadline = Instant::now() + Duration::from_secs(secs);

    let mut frames: u64 = 0;
    let mut bytes: u64 = 0;
    let mut link_up: u64 = 0;
    let mut link_down: u64 = 0;
    let mut area_degraded: u64 = 0;
    let mut other: u64 = 0;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::select! {
            _ = sleep(remaining) => break,
            next = stream.next() => {
                let Some(frame) = next else { break };
                let text = match frame {
                    Ok(Message::Text(text)) => text.as_str().to_owned(),
                    Ok(Message::Binary(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
                    Ok(Message::Close(_)) => break,
                    Ok(_) => continue,
                    Err(_) => break,
                };
                frames += 1;
                bytes += text.len() as u64;
                match kind_of(&text) {
                    Some("LinkUp") => link_up += 1,
                    Some("LinkDown") => link_down += 1,
                    Some("AreaDegraded") => area_degraded += 1,
                    _ => other += 1,
                }
            }
        }
    }

    println!(
        "ws_probe: connected=true frames={frames} bytes={bytes} \
         link_up={link_up} link_down={link_down} area_degraded={area_degraded} other={other}"
    );
}

/// Extract the internally tagged `kind` discriminant from a delta frame without
/// coupling to the `Delta` type: the wire form is `{"kind":"LinkUp",...}`.
fn kind_of(text: &str) -> Option<&'static str> {
    if text.contains(r#""kind":"LinkUp""#) {
        Some("LinkUp")
    } else if text.contains(r#""kind":"LinkDown""#) {
        Some("LinkDown")
    } else if text.contains(r#""kind":"AreaDegraded""#) {
        Some("AreaDegraded")
    } else {
        None
    }
}
