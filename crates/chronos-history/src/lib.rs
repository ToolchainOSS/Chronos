//! Persistent anomaly history for Project Chronos.
//!
//! The engine is stateless and in-memory by default. When history is enabled
//! (`CHRONOS_HISTORY_ENABLED`), this crate durably records detected anomalies to
//! PostgreSQL so a deployment can offer a time-series/history view, while keeping
//! Chronos-owned storage bounded perpetually via daily time partitions plus a
//! hard byte cap.
//!
//! Architecture:
//! - [`HistoryEvent`] is a compact, domain-agnostic record of one anomaly.
//! - [`HistorySink`] abstracts the durable store; [`PostgresSink`] is the sole
//!   implementation today, sitting behind the trait so a future embedded or
//!   remote-write sink can slot in without touching the writer.
//! - [`HistoryWriter`] is a single background task that batches events off the
//!   hot path and enforces retention; the engine pushes through a cheap,
//!   drop-on-full [`HistoryHandle`].
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

mod calendar;
mod config;
mod event;
mod postgres;
mod sink;
mod writer;

pub use config::HistoryConfig;
pub use event::{EventKind, HistoryEvent};
pub use postgres::PostgresSink;
pub use sink::{HistorySink, PruneOutcome, RetentionPolicy};
pub use writer::{HistoryHandle, HistoryWriter};

use std::future::Future;

use tracing::{info, warn};

/// Metric name constants emitted by the history subsystem.
pub mod metrics {
    /// Counter: events successfully written to the store.
    pub const EVENTS_WRITTEN: &str = "chronos_history_events_written_total";
    /// Counter: events dropped (full channel or a failed batch write).
    pub const EVENTS_DROPPED: &str = "chronos_history_events_dropped_total";
    /// Counter: failed batch writes or prune passes.
    pub const WRITE_ERRORS: &str = "chronos_history_write_errors_total";
    /// Gauge: total bytes of Chronos-owned history storage after the last prune.
    pub const STORAGE_BYTES: &str = "chronos_history_storage_bytes";
}

/// Start the history subsystem from configuration.
///
/// Returns `None` when history is disabled or cannot be initialized (an invalid
/// URL is logged and treated as disabled, so a misconfiguration never prevents
/// the engine from running). When `Some`, the background writer task has been
/// spawned and the returned handle records events. The database connection is
/// established lazily on the first write, so startup never blocks on it.
pub fn spawn<F>(config: HistoryConfig, shutdown: F) -> Option<HistoryHandle>
where
    F: Future<Output = ()> + Send + 'static,
{
    if !config.enabled {
        info!("history: disabled (set CHRONOS_HISTORY_ENABLED=true to persist anomalies)");
        return None;
    }
    let sink = match PostgresSink::new(&config.url) {
        Ok(sink) => sink,
        Err(err) => {
            warn!(%err, "history: invalid configuration; history disabled");
            return None;
        }
    };
    let (handle, writer) = HistoryWriter::new(sink, &config);
    tokio::spawn(writer.run(shutdown));
    info!(
        retention_days = config.retention_days,
        max_bytes = config.max_bytes,
        "history: enabled (postgres); connection established lazily on first write"
    );
    Some(handle)
}
