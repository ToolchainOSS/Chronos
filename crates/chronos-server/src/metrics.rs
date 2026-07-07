//! Prometheus metrics installation and metric name constants.

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// Counter: total routing messages processed by the pipeline.
pub const MESSAGES_PROCESSED: &str = "chronos_messages_processed_total";
/// Counter: anomalies detected, labeled by kind.
pub const ANOMALIES_DETECTED: &str = "chronos_anomalies_detected_total";
/// Counter: delta frames broadcast to clients.
pub const DELTAS_BROADCAST: &str = "chronos_deltas_broadcast_total";
/// Gauge: current number of connected WebSocket clients.
pub const CONNECTED_CLIENTS: &str = "chronos_connected_clients";
/// Gauge: current number of ASNs in the topology graph.
pub const GRAPH_NODES: &str = "chronos_graph_nodes";
/// Gauge: current number of edges in the topology graph.
pub const GRAPH_EDGES: &str = "chronos_graph_edges";
/// Counter: ingestion frames dropped due to a full channel.
pub const INGEST_DROPPED: &str = "chronos_ingest_dropped_total";

/// Install the global Prometheus recorder and return a render handle.
pub fn install() -> anyhow::Result<PrometheusHandle> {
    let handle = PrometheusBuilder::new()
        .install_recorder()
        .map_err(|e| anyhow::anyhow!("failed to install Prometheus recorder: {e}"))?;
    Ok(handle)
}

/// Build a standalone render handle without installing a global recorder.
///
/// The global recorder can only be installed once per process, so the
/// acceptance suite (which constructs `AppState` in-process) uses this instead
/// to obtain a `PrometheusHandle` for the `/metrics` endpoint.
pub fn standalone_handle() -> PrometheusHandle {
    PrometheusBuilder::new().build_recorder().handle()
}
