//! The connect, subscribe, read, and reconnect loop.

use crate::config::IngestConfig;
use crate::parse::{parse_message, subscribe_message};
use chronos_types::RisData;
use futures_util::{SinkExt, StreamExt};
use rand::RngExt;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

/// Runtime counters for the ingestion loop; shared so the server can expose them
/// as metrics.
#[derive(Debug, Default)]
pub struct IngestStats {
    /// Total frames read off the socket.
    pub received: AtomicU64,
    /// Frames that parsed into routing messages and were forwarded.
    pub parsed: AtomicU64,
    /// Routing messages dropped because the consumer channel was full.
    pub dropped: AtomicU64,
    /// Frames that failed to parse.
    pub parse_errors: AtomicU64,
    /// Number of reconnect attempts made.
    pub reconnects: AtomicU64,
}

/// Run the ingestion loop until `shutdown` resolves or the consumer disconnects.
///
/// Parsed routing messages are pushed into `tx`. When the channel is full the
/// message is dropped (the `dropped` counter is incremented) so that a slow
/// consumer never blocks the socket reader; this is the backpressure strategy.
pub async fn run_ingest<S>(
    config: IngestConfig,
    tx: mpsc::Sender<RisData>,
    stats: Arc<IngestStats>,
    shutdown: S,
) where
    S: Future<Output = ()>,
{
    tokio::pin!(shutdown);
    let mut backoff = config.min_backoff;

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                info!("ingest: shutdown requested; stopping");
                return;
            }
            outcome = connect_and_stream(&config, &tx, &stats) => {
                match outcome {
                    ConnectionOutcome::ConsumerGone => {
                        info!("ingest: consumer channel closed; stopping");
                        return;
                    }
                    ConnectionOutcome::Disconnected => {
                        stats.reconnects.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }

        // A clean run resets the backoff; otherwise grow it toward the cap.
        let jittered = apply_jitter(backoff);
        warn!(?jittered, "ingest: reconnecting after backoff");
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                info!("ingest: shutdown requested during backoff; stopping");
                return;
            }
            _ = tokio::time::sleep(jittered) => {}
        }
        backoff = (backoff * 2).min(config.max_backoff);
    }
}

enum ConnectionOutcome {
    /// The socket closed or errored; the caller should reconnect.
    Disconnected,
    /// The downstream consumer is gone; the caller should stop entirely.
    ConsumerGone,
}

async fn connect_and_stream(
    config: &IngestConfig,
    tx: &mpsc::Sender<RisData>,
    stats: &Arc<IngestStats>,
) -> ConnectionOutcome {
    info!(url = %config.url, "ingest: connecting to RIS Live");
    let (mut socket, _resp) = match tokio_tungstenite::connect_async(&config.url).await {
        Ok(pair) => pair,
        Err(err) => {
            warn!(%err, "ingest: connection failed");
            return ConnectionOutcome::Disconnected;
        }
    };

    let subscribe = subscribe_message(config.host.as_deref());
    if let Err(err) = socket.send(Message::Text(subscribe.into())).await {
        warn!(%err, "ingest: failed to send subscription");
        return ConnectionOutcome::Disconnected;
    }
    info!("ingest: subscribed to UPDATE stream");

    while let Some(frame) = socket.next().await {
        let message = match frame {
            Ok(message) => message,
            Err(err) => {
                warn!(%err, "ingest: read error");
                return ConnectionOutcome::Disconnected;
            }
        };

        let payload: Vec<u8> = match message {
            Message::Text(text) => text.as_bytes().to_vec(),
            Message::Binary(bytes) => bytes.into(),
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => {
                info!("ingest: server closed the connection");
                return ConnectionOutcome::Disconnected;
            }
            Message::Frame(_) => continue,
        };

        stats.received.fetch_add(1, Ordering::Relaxed);
        match parse_message(&payload) {
            Ok(Some(data)) => match tx.try_send(data) {
                Ok(()) => {
                    stats.parsed.fetch_add(1, Ordering::Relaxed);
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    stats.dropped.fetch_add(1, Ordering::Relaxed);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return ConnectionOutcome::ConsumerGone;
                }
            },
            Ok(None) => {
                debug!("ingest: skipped control frame");
            }
            Err(err) => {
                stats.parse_errors.fetch_add(1, Ordering::Relaxed);
                debug!(%err, "ingest: parse error");
            }
        }
    }

    ConnectionOutcome::Disconnected
}

/// Apply +/- 20 percent jitter to a backoff duration to avoid thundering herds.
fn apply_jitter(base: Duration) -> Duration {
    let millis = base.as_millis() as f64;
    let factor = rand::rng().random_range(0.8..1.2);
    Duration::from_millis((millis * factor) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_within_bounds() {
        let base = Duration::from_millis(1000);
        for _ in 0..1000 {
            let j = apply_jitter(base);
            assert!(j >= Duration::from_millis(800));
            assert!(j <= Duration::from_millis(1200));
        }
    }

    #[tokio::test]
    async fn stops_when_shutdown_fires_immediately() {
        let (tx, _rx) = mpsc::channel(4);
        let stats = Arc::new(IngestStats::default());
        // A config pointing at an unroutable address; shutdown wins the race.
        let config = IngestConfig {
            url: "ws://127.0.0.1:1/".to_string(),
            ..Default::default()
        };
        // Should return promptly because shutdown is already ready.
        tokio::time::timeout(
            Duration::from_secs(5),
            run_ingest(config, tx, stats, async {}),
        )
        .await
        .expect("run_ingest did not observe shutdown");
    }
}
