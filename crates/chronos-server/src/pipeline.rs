//! The ingestion consumer pipeline (blueprint Phase 2 Task 2.1 consumer side, and
//! Phase 3 detection wiring).
//!
//! A single consumer task drains the bounded ingestion channel, updates the
//! topology structures, runs the detection heuristics, and broadcasts the
//! resulting minimal delta frames. A companion sweep interval ages out stale AS
//! edges and refreshes the graph gauges.

use crate::metrics as m;
use chronos_detect::{Anomaly, PathLeakDetector, RelationshipProvider, SurgeMonitor, check_origin};
use chronos_geo::GeoResolver;
use chronos_ingest::IngestStats;
use chronos_topology::{AsGraph, PrefixTable};
use chronos_types::{Delta, RisData};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc};
use tracing::info;

/// Shared handles the pipeline needs to run.
pub struct Pipeline {
    graph: Arc<AsGraph>,
    prefixes: Arc<PrefixTable>,
    leak: PathLeakDetector<Arc<dyn RelationshipProvider>>,
    surge: SurgeMonitor,
    geo: Arc<GeoResolver>,
    deltas: broadcast::Sender<Delta>,
    ingest_stats: Arc<IngestStats>,
}

impl Pipeline {
    /// Construct a pipeline.
    pub fn new(
        graph: Arc<AsGraph>,
        prefixes: Arc<PrefixTable>,
        relationships: Arc<dyn RelationshipProvider>,
        surge: SurgeMonitor,
        geo: Arc<GeoResolver>,
        deltas: broadcast::Sender<Delta>,
        ingest_stats: Arc<IngestStats>,
    ) -> Self {
        Self {
            graph,
            prefixes,
            leak: PathLeakDetector::new(relationships),
            surge,
            geo,
            deltas,
            ingest_stats,
        }
    }

    /// Run until the channel closes or `shutdown` resolves.
    pub async fn run<S>(
        mut self,
        mut rx: mpsc::Receiver<RisData>,
        edge_ttl: Duration,
        sweep_interval: Duration,
        shutdown: S,
    ) where
        S: Future<Output = ()>,
    {
        tokio::pin!(shutdown);
        let mut sweep = tokio::time::interval(sweep_interval);
        let mut last_dropped = 0u64;

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => {
                    info!("pipeline: shutdown requested; stopping");
                    return;
                }
                maybe = rx.recv() => {
                    match maybe {
                        Some(data) => self.process(data),
                        None => {
                            info!("pipeline: ingestion channel closed; stopping");
                            return;
                        }
                    }
                }
                _ = sweep.tick() => {
                    self.on_sweep(edge_ttl, &mut last_dropped);
                }
            }
        }
    }

    fn process(&mut self, data: RisData) {
        metrics::counter!(m::MESSAGES_PROCESSED).increment(1);
        let now = event_time(data.timestamp);

        // Topology: derive peering edges from the AS_PATH.
        for (a, b) in self.graph.observe_path(&data.path, now) {
            self.broadcast(Delta::link_up(a, b));
        }

        // Path leak heuristic (no per prefix context needed).
        if let Some(anomaly) = self.leak.inspect(&data.path) {
            self.handle_anomaly(anomaly);
        }

        // Per prefix heuristics for announcements.
        let origin = data.origin_asn();
        for prefix in data.announced_prefixes() {
            if let Some(origin) = origin {
                let obs = self.prefixes.observe(&prefix, origin);
                if let Some(anomaly) = check_origin(&prefix, &obs) {
                    self.handle_anomaly(anomaly);
                }
            }
            if let Some(anomaly) = self.surge.record(prefix, now) {
                self.handle_anomaly(anomaly);
            }
        }

        // Withdrawals: forget the origin so a later re-announcement is treated as
        // fresh rather than a false origin change.
        for prefix in &data.withdrawals {
            self.prefixes.remove(prefix);
        }
    }

    fn on_sweep(&mut self, edge_ttl: Duration, last_dropped: &mut u64) {
        let now = event_time(0.0);
        for (a, b) in self.graph.sweep_expired(now, edge_ttl.as_secs_f64()) {
            self.broadcast(Delta::link_down(a, b));
        }
        self.surge.evict_stale(now);

        // Refresh graph gauges.
        metrics::gauge!(m::GRAPH_NODES).set(self.graph.node_count() as f64);
        metrics::gauge!(m::GRAPH_EDGES).set(self.graph.edge_count() as f64);

        // Reflect ingestion drops into the counter (monotonic increments only).
        let dropped = self.ingest_stats.dropped.load(Ordering::Relaxed);
        if dropped > *last_dropped {
            metrics::counter!(m::INGEST_DROPPED).increment(dropped - *last_dropped);
            *last_dropped = dropped;
        }
    }

    fn handle_anomaly(&self, anomaly: Anomaly) {
        let kind = match &anomaly {
            Anomaly::PrefixHijack { .. } => "prefix_hijack",
            Anomaly::PathLeak { .. } => "path_leak",
            Anomaly::RouteChurn { .. } => "route_churn",
        };
        metrics::counter!(m::ANOMALIES_DETECTED, "kind" => kind).increment(1);

        // Map the anomaly onto a geographic region when possible so the frontend
        // map can highlight the affected area.
        if let Some(prefix) = anomaly.prefix() {
            if let Some(region) = self.geo.resolve_region(&prefix) {
                let severity = anomaly.severity().as_index();
                self.broadcast(Delta::area_degraded(region, severity));
            }
        }
    }

    fn broadcast(&self, delta: Delta) {
        // A send error only means there are no subscribers right now; that is fine.
        if self.deltas.send(delta).is_ok() {
            metrics::counter!(m::DELTAS_BROADCAST).increment(1);
        }
    }
}

/// Resolve an event timestamp: use the collector timestamp when present and
/// plausible, otherwise fall back to wall clock time.
fn event_time(timestamp: f64) -> f64 {
    if timestamp > 0.0 {
        timestamp
    } else {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }
}
