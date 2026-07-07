//! The `HistorySink` abstraction: where recorded events are durably stored and
//! aged out.
//!
//! The trait uses native `async fn` (return-position `impl Future`) rather than
//! dynamic dispatch: the writer task is generic over one concrete sink, so calls
//! monomorphize with no boxing on the write path. A future embedded or
//! remote-write sink implements the same trait without touching the writer.

use std::future::Future;

use crate::event::HistoryEvent;

/// Retention limits applied when a sink prunes old data.
#[derive(Debug, Clone, Copy)]
pub struct RetentionPolicy {
    /// Current time as Unix epoch seconds (the reference point for age based
    /// pruning). Passed in rather than read from the clock so pruning is
    /// deterministic and testable.
    pub now: f64,
    /// Drop whole days strictly older than this many days before `now`.
    pub retention_days: u32,
    /// Hard ceiling on total storage for Chronos-owned history, in bytes. When
    /// exceeded, the oldest data is dropped until the store fits again.
    pub max_bytes: u64,
}

/// The outcome of a prune pass, used to surface storage metrics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneOutcome {
    /// Number of daily partitions dropped in this pass.
    pub dropped_partitions: usize,
    /// Total bytes occupied by the history after pruning.
    pub total_bytes: u64,
}

/// A durable destination for anomaly history.
///
/// Implementations own whatever connection or handle they need and are driven
/// exclusively by a single writer task, so `&mut self` methods are free to
/// reconnect or mutate internal caches without synchronization.
pub trait HistorySink: Send + 'static {
    /// Persist a batch of events. Called with at least one event. Errors are
    /// logged and the batch is dropped by the writer (history never blocks or
    /// crashes the engine), so implementations should attempt reconnection on
    /// the next call rather than retaining state across a failure.
    fn record_events(
        &mut self,
        events: &[HistoryEvent],
    ) -> impl Future<Output = anyhow::Result<()>> + Send;

    /// Enforce the retention policy, returning what was pruned and the resulting
    /// total size.
    fn prune(
        &mut self,
        policy: &RetentionPolicy,
    ) -> impl Future<Output = anyhow::Result<PruneOutcome>> + Send;
}
