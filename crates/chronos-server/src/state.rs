//! Shared application state handed to the Axum handlers.

use chronos_topology::AsGraph;
use chronos_types::Delta;
use metrics_exporter_prometheus::PrometheusHandle;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::broadcast;

/// Cloneable shared state (all heavy fields are behind `Arc`).
#[derive(Clone)]
pub struct AppState {
    /// Broadcast sender for delta frames; each client subscribes a receiver.
    pub deltas: broadcast::Sender<Delta>,
    /// The live topology graph (used to build initial client snapshots).
    pub graph: Arc<AsGraph>,
    /// Prometheus render handle for the `/metrics` endpoint.
    pub metrics: Arc<PrometheusHandle>,
    /// Maximum number of edges to include in an initial snapshot.
    pub snapshot_max: usize,
    /// Readiness flag flipped true once ingestion has started.
    pub ready: Arc<AtomicBool>,
}
