//! Chronos soak / baseline harness entry point.
//!
//! Runs the release `chronos-server` against the real RIPE RIS Live feed for a
//! measurement window and produces a self-contained Markdown assurance report
//! (console log summary, resource usage, and performance), plus a CSV time
//! series. A short run is the resource baseline; a long run is the production
//! soak. This is a measurement tool, not a unit test or a PR gate; it needs
//! outbound network to RIS, CAIDA, and the GeoLite2 mirror.
//!
//! Orchestration: acquire geo -> spawn server -> wait ready -> warmup -> run a
//! concurrent monitor (samples resource + engine counters) and a WebSocket
//! egress probe -> stop server -> aggregate -> render report -> exit with a
//! verdict code (0 pass/warn, 2 fail on crash/panic).
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

mod config;
mod metrics;
mod monitor;
mod proc;
mod report;
mod server;
mod wsprobe;

use std::time::Duration;

use config::Config;
use report::{Aggregate, LogAnalysis, ReportContext};

/// WebSocket egress probe is capped at this many seconds regardless of window
/// length: draining the firehose to a client for the whole of a 20-minute soak
/// adds no signal beyond the first few minutes.
const MAX_PROBE_SECS: u64 = 300;

/// Readiness timeout: CAIDA discovery plus the RIS handshake should complete
/// well within this.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let repo_root = std::env::current_dir()?;
    let config = Config::from_args_and_env(&repo_root)?;

    std::fs::create_dir_all(config.data_dir())?;
    let nproc = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1) as u64;
    let clk_tck = proc::clk_tck();

    eprintln!("== Chronos soak / baseline harness ==");
    eprintln!("binary:   {}", config.bin.display());
    eprintln!("host:     {nproc} vCPU, CLK_TCK={clk_tck}");
    eprintln!("out dir:  {}", config.out_dir.display());
    eprintln!(
        "window:   {}s warmup + {}s measured ({}s sampling)",
        config.warmup.as_secs(),
        config.duration.as_secs(),
        config.interval.as_secs()
    );
    eprintln!(
        "RIS feed: {}",
        config.ris_host.as_deref().unwrap_or("unfiltered firehose")
    );

    // 1. Optional GeoLite2 acquisition (a typical instance runs geo enabled).
    eprintln!("-- acquiring GeoLite2 (geo enabled path) --");
    let geo = server::acquire_geo(&config, &config.data_dir()).await;
    eprintln!("   {}", geo.describe());

    // 2. Spawn the server with full INFO logging captured to a file.
    let log_path = config.log_path();
    eprintln!("-- starting chronos-server against the real RIS Live feed --");
    let mut child = server::spawn(&config, &geo, &log_path)?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("failed to obtain server pid"))?;

    // 3. Readiness gate.
    if let Err(err) = server::wait_ready(&mut child, READY_TIMEOUT).await {
        let _ = child.kill().await;
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        eprintln!("error: {err}\n--- server log tail ---");
        for line in log.lines().rev().take(30).collect::<Vec<_>>().iter().rev() {
            eprintln!("{line}");
        }
        anyhow::bail!("server did not start");
    }
    eprintln!(
        "-- ready (pid {pid}); warmup {}s --",
        config.warmup.as_secs()
    );
    tokio::time::sleep(config.warmup).await;

    // 4. Concurrent measurement: monitor samples resource + engine counters
    //    while the egress probe exercises the WebSocket path end to end.
    let client = reqwest::Client::new();
    let probe_secs = config.duration.as_secs().min(MAX_PROBE_SECS);
    eprintln!(
        "-- measuring for {}s ({} samples @ {}s), WS probe {}s --",
        config.duration.as_secs(),
        config.sample_count(),
        config.interval.as_secs(),
        probe_secs
    );
    let probe_handle = tokio::spawn(wsprobe::run(Duration::from_secs(probe_secs)));
    let monitored = monitor::run(
        &mut child,
        pid,
        &client,
        config.sample_count(),
        config.interval,
    )
    .await;
    let ws = probe_handle.await.unwrap_or_default();

    // 5. Snapshot liveness, then stop the server.
    let server_died = monitored.server_died || matches!(child.try_wait(), Ok(Some(_)) | Err(_));
    let _ = child.kill().await;
    let _ = child.wait().await;

    // 6. Aggregate and render.
    monitor::write_csv(&config.csv_path(), &monitored.samples)?;
    let log_text = std::fs::read_to_string(&log_path).unwrap_or_default();
    let agg = Aggregate::from_samples(&monitored.samples, clk_tck, nproc);
    let log = LogAnalysis::from_log(&log_text);
    let (verdict, notes) = report::decide_verdict(&agg, &log, server_died);

    let ctx = ReportContext {
        config: &config,
        geo: &geo,
        ws: &ws,
        nproc,
        clk_tck,
        verdict,
        notes: &notes,
    };
    let markdown = report::render(&agg, &log, &monitored.samples, &ctx);
    std::fs::write(&config.report_path, &markdown)?;

    eprintln!();
    eprintln!(
        "{}",
        report::console_summary(&agg, verdict, config.duration)
    );
    eprintln!("report:  {}", config.report_path.display());
    eprintln!("log:     {}", log_path.display());
    eprintln!("samples: {}", config.csv_path().display());

    std::process::exit(verdict.exit_code());
}
