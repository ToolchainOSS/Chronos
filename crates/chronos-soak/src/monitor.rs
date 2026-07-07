//! The sampling monitor: periodically snapshots the server's resource use and
//! engine counters into a time series, and writes it out as CSV.
//!
//! This is the "separate observer" the report describes: it runs concurrently
//! with the server under test and the egress probe, reading kernel accounting
//! and scraping `/metrics` on a fixed cadence so memory growth, CPU, and
//! throughput are visible over the whole window rather than only at the ends.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use tokio::process::Child;
use tokio::time::{Instant, sleep};

use crate::metrics::MetricsSnapshot;
use crate::{proc, server};

/// One measurement point.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    /// Seconds since the measurement window began.
    pub elapsed_s: u64,
    /// Resident set size in kibibytes.
    pub rss_kb: u64,
    /// Cumulative CPU ticks (`utime + stime`).
    pub cpu_ticks: u64,
    /// Cumulative RIS socket bytes received, or `None` if unmeasured.
    pub sock_rx: Option<u64>,
    /// Engine counters at this instant.
    pub metrics: MetricsSnapshot,
}

/// Result of a completed monitoring window.
pub struct MonitorResult {
    /// All collected samples, in order.
    pub samples: Vec<Sample>,
    /// Whether the server process was observed to die mid-window.
    pub server_died: bool,
}

/// Run the sampling loop for `samples` iterations at `interval`, watching `pid`
/// and scraping metrics via `client`. Stops early if the child exits.
pub async fn run(
    child: &mut Child,
    pid: u32,
    client: &reqwest::Client,
    samples: u64,
    interval: Duration,
) -> MonitorResult {
    let start = Instant::now();
    let mut collected = Vec::with_capacity(samples as usize);
    let mut server_died = false;

    for _ in 0..samples {
        sleep(interval).await;
        if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
            server_died = true;
            break;
        }
        let metrics = server::scrape_metrics(client).await;
        collected.push(Sample {
            elapsed_s: start.elapsed().as_secs(),
            rss_kb: proc::rss_kb(pid).unwrap_or(0),
            cpu_ticks: proc::cpu_ticks(pid).unwrap_or(0),
            sock_rx: proc::ris_socket_rx_bytes(pid),
            metrics,
        });
    }

    MonitorResult {
        samples: collected,
        server_died,
    }
}

/// Write the collected samples as CSV for artifact upload and offline analysis.
pub fn write_csv(path: &Path, samples: &[Sample]) -> anyhow::Result<()> {
    let mut file = std::fs::File::create(path)?;
    writeln!(
        file,
        "elapsed_s,rss_kb,cpu_ticks,sock_rx,msgs,hijack,leak,churn,nodes,edges,dropped,deltas,clients"
    )?;
    for s in samples {
        let m = &s.metrics;
        writeln!(
            file,
            "{},{},{},{},{},{},{},{},{},{},{},{},{}",
            s.elapsed_s,
            s.rss_kb,
            s.cpu_ticks,
            s.sock_rx.map(|v| v as i64).unwrap_or(-1),
            m.messages,
            m.hijack,
            m.leak,
            m.churn,
            m.nodes,
            m.edges,
            m.dropped,
            m.deltas,
            m.clients,
        )?;
    }
    Ok(())
}
