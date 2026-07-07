//! WebSocket egress probe, run as a background task for a bounded slice of the
//! window. Draining `/ws` proves the full ingest -> detect -> broadcast ->
//! egress path and makes `chronos_deltas_broadcast_total` /
//! `chronos_connected_clients` meaningful (the server skips the broadcast when
//! no client is subscribed).
//!
//! Egress is a bonus assurance signal, not a hard gate: a connect failure is
//! reported, not fatal.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

use std::time::Duration;

use futures_util::StreamExt;
use tokio::time::{Instant, sleep};
use tokio_tungstenite::tungstenite::Message;

use crate::config::BIND_ADDR;

/// Result of the egress probe.
#[derive(Debug, Clone, Default)]
pub struct WsProbe {
    /// Whether the WebSocket connected.
    pub connected: bool,
    /// A connect error message, when the probe failed to attach.
    pub error: Option<String>,
    /// Delta frames received.
    pub frames: u64,
    /// Total payload bytes received.
    pub bytes: u64,
    /// `LinkUp` frames (dominated by the initial snapshot).
    pub link_up: u64,
    /// `LinkDown` frames.
    pub link_down: u64,
    /// `AreaDegraded` frames (require geo enabled and an anomaly with a region).
    pub area_degraded: u64,
}

impl WsProbe {
    /// One-line report summary.
    pub fn summary(&self) -> String {
        if !self.connected {
            return format!(
                "not connected{}",
                self.error
                    .as_ref()
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default()
            );
        }
        format!(
            "{} frames, {} bytes (LinkUp {}, LinkDown {}, AreaDegraded {})",
            self.frames, self.bytes, self.link_up, self.link_down, self.area_degraded
        )
    }
}

/// Connect to `/ws` and drain frames for `duration`, classifying each by its
/// internally tagged `kind`.
pub async fn run(duration: Duration) -> WsProbe {
    let url = format!("ws://{BIND_ADDR}/ws");
    let socket = match tokio_tungstenite::connect_async(&url).await {
        Ok((socket, _resp)) => socket,
        Err(err) => {
            return WsProbe {
                connected: false,
                error: Some(err.to_string()),
                ..WsProbe::default()
            };
        }
    };

    let (_sink, mut stream) = socket.split();
    let deadline = Instant::now() + duration;
    let mut probe = WsProbe {
        connected: true,
        ..WsProbe::default()
    };

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
                probe.frames += 1;
                probe.bytes += text.len() as u64;
                classify(&text, &mut probe);
            }
        }
    }
    probe
}

/// Bump the per-kind counter for a delta frame. The wire form is internally
/// tagged: `{"kind":"LinkUp",...}`.
fn classify(text: &str, probe: &mut WsProbe) {
    if text.contains(r#""kind":"LinkUp""#) {
        probe.link_up += 1;
    } else if text.contains(r#""kind":"LinkDown""#) {
        probe.link_down += 1;
    } else if text.contains(r#""kind":"AreaDegraded""#) {
        probe.area_degraded += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_each_kind() {
        let mut p = WsProbe::default();
        classify(r#"{"kind":"LinkUp","a":1,"b":2}"#, &mut p);
        classify(r#"{"kind":"LinkDown","a":1,"b":2}"#, &mut p);
        classify(
            r#"{"kind":"AreaDegraded","region":"US","severity":0.5}"#,
            &mut p,
        );
        assert_eq!((p.link_up, p.link_down, p.area_degraded), (1, 1, 1));
    }

    #[test]
    fn summary_reports_not_connected() {
        let p = WsProbe {
            connected: false,
            error: Some("refused".to_string()),
            ..WsProbe::default()
        };
        assert!(p.summary().contains("not connected"));
        assert!(p.summary().contains("refused"));
    }
}
