//! The background writer task and its producer handle.
//!
//! The engine never touches the database directly. It hands finished
//! [`HistoryEvent`]s to a [`HistoryHandle`], a cheap cloneable producer that
//! `try_send`s into a bounded channel and drops on a full channel (the same
//! backpressure discipline as RIS ingestion: never block or slow the hot path).
//! A single [`HistoryWriter`] task drains that channel, batches events, flushes
//! them to the sink, and periodically prunes to enforce retention.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use crate::config::HistoryConfig;
use crate::event::HistoryEvent;
use crate::metrics as m;
use crate::sink::{HistorySink, RetentionPolicy};

/// A cheap, cloneable producer used by the engine to record events without ever
/// blocking. Events are dropped (and counted) when the writer cannot keep up.
#[derive(Clone)]
pub struct HistoryHandle {
    tx: mpsc::Sender<HistoryEvent>,
    dropped: Arc<AtomicU64>,
}

impl HistoryHandle {
    /// Record one event. Non-blocking: on a full channel the event is dropped
    /// and the drop counter (and metric) is incremented; on a closed channel the
    /// event is silently discarded.
    pub fn record(&self, event: HistoryEvent) {
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                metrics::counter!(m::EVENTS_DROPPED).increment(1);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {}
        }
    }

    /// Total events dropped so far due to a full channel.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// The background task that batches events and writes them to a sink.
pub struct HistoryWriter<S: HistorySink> {
    sink: S,
    rx: mpsc::Receiver<HistoryEvent>,
    batch_size: usize,
    flush_interval: Duration,
    prune_interval: Duration,
    retention_days: u32,
    max_bytes: u64,
}

impl<S: HistorySink> HistoryWriter<S> {
    /// Build the channel, handle, and writer for a sink and configuration.
    pub fn new(sink: S, config: &HistoryConfig) -> (HistoryHandle, Self) {
        let (tx, rx) = mpsc::channel(config.channel_bound);
        let handle = HistoryHandle {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
        };
        let writer = Self {
            sink,
            rx,
            batch_size: config.batch_size,
            flush_interval: config.flush_interval,
            prune_interval: config.prune_interval,
            retention_days: config.retention_days,
            max_bytes: config.max_bytes,
        };
        (handle, writer)
    }

    /// Run until the channel closes or `shutdown` resolves, flushing any
    /// buffered events on the way out.
    pub async fn run<F>(mut self, shutdown: F)
    where
        F: Future<Output = ()>,
    {
        tokio::pin!(shutdown);
        let mut flush = interval(self.flush_interval);
        flush.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut prune = interval(self.prune_interval);
        prune.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // The first tick of each interval fires immediately; consume them so the
        // loop does not flush an empty buffer or prune an empty store at startup.
        flush.tick().await;
        prune.tick().await;

        let mut buffer: Vec<HistoryEvent> = Vec::with_capacity(self.batch_size);

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => {
                    info!("history: shutdown requested; flushing and stopping");
                    while let Ok(event) = self.rx.try_recv() {
                        buffer.push(event);
                    }
                    self.flush(&mut buffer).await;
                    return;
                }
                maybe = self.rx.recv() => {
                    match maybe {
                        Some(event) => {
                            buffer.push(event);
                            if buffer.len() >= self.batch_size {
                                self.flush(&mut buffer).await;
                            }
                        }
                        None => {
                            info!("history: event channel closed; flushing and stopping");
                            self.flush(&mut buffer).await;
                            return;
                        }
                    }
                }
                _ = flush.tick() => {
                    if !buffer.is_empty() {
                        self.flush(&mut buffer).await;
                    }
                }
                _ = prune.tick() => {
                    self.prune().await;
                }
            }
        }
    }

    /// Flush the buffered events to the sink. On error the batch is dropped (and
    /// counted): history must never stall or crash the engine, so we favor
    /// bounded memory over unbounded retries.
    async fn flush(&mut self, buffer: &mut Vec<HistoryEvent>) {
        if buffer.is_empty() {
            return;
        }
        let count = buffer.len() as u64;
        match self.sink.record_events(buffer).await {
            Ok(()) => metrics::counter!(m::EVENTS_WRITTEN).increment(count),
            Err(err) => {
                warn!(%err, count, "history: batch write failed; dropping batch");
                metrics::counter!(m::WRITE_ERRORS).increment(1);
                metrics::counter!(m::EVENTS_DROPPED).increment(count);
            }
        }
        buffer.clear();
    }

    /// Run one retention pass, updating the storage gauge on success.
    async fn prune(&mut self) {
        let policy = RetentionPolicy {
            now: now_epoch_secs(),
            retention_days: self.retention_days,
            max_bytes: self.max_bytes,
        };
        match self.sink.prune(&policy).await {
            Ok(outcome) => {
                metrics::gauge!(m::STORAGE_BYTES).set(outcome.total_bytes as f64);
                if outcome.dropped_partitions > 0 {
                    info!(
                        dropped = outcome.dropped_partitions,
                        total_bytes = outcome.total_bytes,
                        "history: pruned old partitions"
                    );
                }
            }
            Err(err) => {
                warn!(%err, "history: prune failed");
                metrics::counter!(m::WRITE_ERRORS).increment(1);
            }
        }
    }
}

/// Current wall-clock time as Unix epoch seconds.
fn now_epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
