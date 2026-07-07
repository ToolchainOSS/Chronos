//! Project Chronos server entry point.
//!
//! Wiring (blueprint Phases 1 to 4):
//! - Build the shared topology structures and the delta broadcast channel.
//! - Select an AS relationship provider (mounted CAIDA dataset when configured,
//!   otherwise a degree based heuristic over the live graph).
//! - Load the GeoLite2 resolver from mounted databases (disabled gracefully when
//!   the files are absent).
//! - Spawn the RIS Live ingestion task (producer) and the detection pipeline
//!   (consumer), connected by a bounded channel.
//! - Serve the Axum egress (WebSocket plus health and metrics) with graceful
//!   shutdown on SIGINT or SIGTERM.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

mod config;
mod hub;
mod metrics;
mod pipeline;
mod state;

use crate::config::AppConfig;
use crate::pipeline::Pipeline;
use crate::state::AppState;
use chronos_detect::{
    DegreeHeuristic, RelationshipProvider, SurgeConfig, SurgeMonitor, parse_caida_as_rel,
};
use chronos_geo::GeoResolver;
use chronos_ingest::{IngestConfig, IngestStats};
use chronos_topology::{AsGraph, PrefixTable};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let config = AppConfig::from_env()?;
    info!(?config, "chronos: starting");

    let metrics_handle = Arc::new(metrics::install()?);

    // Shared topology structures.
    let graph = Arc::new(AsGraph::new());
    let prefixes = Arc::new(PrefixTable::new());

    // AS relationship provider selection.
    let relationships = build_relationship_provider(&config, graph.clone());

    // Geo resolver from mounted databases (degrades gracefully when absent).
    let geo = Arc::new(GeoResolver::load(
        config.geolite2_city_db.as_deref(),
        config.geolite2_asn_db.as_deref(),
    ));
    if geo.is_region_enabled() {
        info!("chronos: geo region resolution enabled");
    } else {
        warn!(
            "chronos: geo region resolution disabled (no GeoLite2 City database mounted); \
             AreaDegraded deltas will not be emitted"
        );
    }

    // Channels.
    let (ingest_tx, ingest_rx) = mpsc::channel(config.ingest_channel_bound);
    let (delta_tx, _delta_rx) = broadcast::channel(config.broadcast_capacity);

    // Shutdown fan out.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Ingestion task (producer).
    let ingest_stats = Arc::new(IngestStats::default());
    let ingest_config = IngestConfig {
        url: config.ris_url.clone(),
        host: config.ris_host.clone(),
        ..IngestConfig::default()
    };
    let ingest_handle = tokio::spawn(chronos_ingest::run_ingest(
        ingest_config,
        ingest_tx,
        ingest_stats.clone(),
        wait_shutdown(shutdown_rx.clone()),
    ));

    // Detection pipeline (consumer).
    let surge = SurgeMonitor::new(SurgeConfig::default());
    let pipeline = Pipeline::new(
        graph.clone(),
        prefixes.clone(),
        relationships,
        surge,
        geo.clone(),
        delta_tx.clone(),
        ingest_stats.clone(),
    );
    let pipeline_handle = tokio::spawn(pipeline.run(
        ingest_rx,
        config.edge_ttl,
        config.sweep_interval,
        wait_shutdown(shutdown_rx.clone()),
    ));

    // HTTP and WebSocket egress.
    let ready = Arc::new(AtomicBool::new(true));
    let app_state = AppState {
        deltas: delta_tx,
        graph: graph.clone(),
        metrics: metrics_handle,
        snapshot_max: config.snapshot_max,
        ready,
    };
    let app = hub::router(app_state);

    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    info!(bind = %config.bind_addr, "chronos: listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_tx))
        .await?;

    // Give the background tasks a chance to observe shutdown and stop.
    let _ = ingest_handle.await;
    let _ = pipeline_handle.await;
    info!("chronos: stopped");
    Ok(())
}

fn build_relationship_provider(
    config: &AppConfig,
    graph: Arc<AsGraph>,
) -> Arc<dyn RelationshipProvider> {
    if let Some(path) = &config.caida_as_rel {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let rels = parse_caida_as_rel(&contents);
                info!(
                    path = %path.display(),
                    entries = rels.len(),
                    "chronos: loaded CAIDA AS relationships"
                );
                return Arc::new(rels);
            }
            Err(err) => {
                warn!(
                    path = %path.display(),
                    %err,
                    "chronos: failed to read CAIDA dataset; falling back to degree heuristic"
                );
            }
        }
    } else {
        info!(
            "chronos: no CAIDA dataset configured; using degree based relationship heuristic \
             (set CHRONOS_CAIDA_ASREL to a mounted dataset for higher fidelity leak detection)"
        );
    }
    Arc::new(DegreeHeuristic::new(graph, config.degree_ratio))
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,chronos_ingest=info,chronos_server=info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}

/// Resolve when the shutdown signal flips to true.
async fn wait_shutdown(mut rx: watch::Receiver<bool>) {
    // If already signaled, return immediately.
    if *rx.borrow() {
        return;
    }
    while rx.changed().await.is_ok() {
        if *rx.borrow() {
            return;
        }
    }
}

/// Wait for SIGINT or SIGTERM, then broadcast shutdown to the background tasks.
async fn shutdown_signal(shutdown_tx: watch::Sender<bool>) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    info!("chronos: shutdown signal received");
    let _ = shutdown_tx.send(true);
}
