//! The ingestion consumer pipeline (blueprint Phase 2 Task 2.1 consumer side, and
//! Phase 3 detection wiring).
//!
//! A single consumer task drains the bounded ingestion channel, updates the
//! topology structures, runs the detection heuristics, and broadcasts the
//! resulting minimal delta frames. A companion sweep interval ages out stale AS
//! edges and refreshes the graph gauges.

use crate::metrics as m;
use chronos_detect::{
    Anomaly, PathLeakDetector, RelationshipProvider, Severity, SurgeMonitor, check_origin,
};
use chronos_geo::GeoResolver;
use chronos_history::{EventKind, HistoryEvent, HistoryHandle};
use chronos_ingest::IngestStats;
use chronos_topology::{AsGraph, PrefixTable};
use chronos_types::{Delta, RisData};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc};
use tracing::info;

/// The dependencies a [`Pipeline`] is built from. Grouped into a struct so the
/// wiring reads by field name and stays clear as the pipeline grows.
pub struct PipelineDeps {
    /// The shared AS peering graph.
    pub graph: Arc<AsGraph>,
    /// The shared prefix-to-origin table.
    pub prefixes: Arc<PrefixTable>,
    /// The AS relationship provider used by the path-leak detector.
    pub relationships: Arc<dyn RelationshipProvider>,
    /// The route-churn surge monitor.
    pub surge: SurgeMonitor,
    /// The geo resolver used to map anomalies to regions.
    pub geo: Arc<GeoResolver>,
    /// The delta broadcast sender.
    pub deltas: broadcast::Sender<Delta>,
    /// Shared ingestion statistics (for the dropped-frame counter).
    pub ingest_stats: Arc<IngestStats>,
    /// Optional history handle; `None` when persistence is disabled.
    pub history: Option<HistoryHandle>,
}

/// Shared handles the pipeline needs to run.
pub struct Pipeline {
    graph: Arc<AsGraph>,
    prefixes: Arc<PrefixTable>,
    leak: PathLeakDetector<Arc<dyn RelationshipProvider>>,
    surge: SurgeMonitor,
    geo: Arc<GeoResolver>,
    deltas: broadcast::Sender<Delta>,
    ingest_stats: Arc<IngestStats>,
    history: Option<HistoryHandle>,
}

impl Pipeline {
    /// Construct a pipeline from its dependencies.
    pub fn new(deps: PipelineDeps) -> Self {
        Self {
            graph: deps.graph,
            prefixes: deps.prefixes,
            leak: PathLeakDetector::new(deps.relationships),
            surge: deps.surge,
            geo: deps.geo,
            deltas: deps.deltas,
            ingest_stats: deps.ingest_stats,
            history: deps.history,
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
            self.handle_anomaly(anomaly, now);
        }

        // Per prefix heuristics for announcements.
        let origin = data.origin_asn();
        for prefix in data.announced_prefixes() {
            if let Some(origin) = origin {
                let obs = self.prefixes.observe(&prefix, origin);
                if let Some(anomaly) = check_origin(&prefix, &obs) {
                    self.handle_anomaly(anomaly, now);
                }
            }
            if let Some(anomaly) = self.surge.record(prefix, now) {
                self.handle_anomaly(anomaly, now);
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

    fn handle_anomaly(&self, anomaly: Anomaly, now: f64) {
        let kind = match &anomaly {
            Anomaly::PrefixHijack { .. } => "prefix_hijack",
            Anomaly::PathLeak { .. } => "path_leak",
            Anomaly::RouteChurn { .. } => "route_churn",
        };
        metrics::counter!(m::ANOMALIES_DETECTED, "kind" => kind).increment(1);

        // Map the anomaly onto a geographic region when possible so the frontend
        // map can highlight the affected area.
        let region = anomaly
            .prefix()
            .and_then(|prefix| self.geo.resolve_region(&prefix));
        if let Some(region) = &region {
            let severity = anomaly.severity().as_index();
            self.broadcast(Delta::area_degraded(region.clone(), severity));
        }

        // Persist the anomaly for the history/time-series view when history is
        // enabled. This is a non-blocking, drop-on-full record: it never slows or
        // stalls the detection path.
        if let Some(history) = &self.history {
            history.record(build_history_event(&anomaly, region, now));
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

/// Ordinal severity used by the history store: 0 = Low, 1 = Medium, 2 = High.
fn severity_ordinal(severity: Severity) -> i16 {
    match severity {
        Severity::Low => 0,
        Severity::Medium => 1,
        Severity::High => 2,
    }
}

/// Translate a domain [`Anomaly`] into the storage-facing [`HistoryEvent`],
/// carrying the already-resolved region so the mapping does not repeat geo work.
fn build_history_event(anomaly: &Anomaly, region: Option<String>, now: f64) -> HistoryEvent {
    let mut event = HistoryEvent::new(now, EventKind::PrefixHijack, 0);
    event.region = region;
    match anomaly {
        Anomaly::PrefixHijack {
            prefix,
            previous_origin,
            new_origin,
            severity,
        } => {
            event.kind = EventKind::PrefixHijack;
            event.severity = severity_ordinal(*severity);
            event.prefix = Some(prefix.to_string());
            event.previous_origin = Some(i64::from(previous_origin.value()));
            event.new_origin = Some(i64::from(new_origin.value()));
        }
        Anomaly::PathLeak {
            path,
            offending_asn,
            severity,
        } => {
            event.kind = EventKind::PathLeak;
            event.severity = severity_ordinal(*severity);
            event.offending_asn = Some(i64::from(offending_asn.value()));
            event.as_path = Some(path.iter().map(|asn| i64::from(asn.value())).collect());
        }
        Anomaly::RouteChurn {
            prefix,
            updates_in_window,
            threshold,
            severity,
        } => {
            event.kind = EventKind::RouteChurn;
            event.severity = severity_ordinal(*severity);
            event.prefix = Some(prefix.to_string());
            event.updates_in_window = Some(i32::try_from(*updates_in_window).unwrap_or(i32::MAX));
            event.threshold = Some(*threshold);
        }
    }
    event
}
