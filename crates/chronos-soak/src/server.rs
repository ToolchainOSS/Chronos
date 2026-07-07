//! Managing the server under test: optional GeoLite2 download, spawning the
//! release binary with full INFO logging captured to a file, and readiness
//! polling.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

use std::fs::File;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::time::sleep;

use crate::config::{BIND_ADDR, Config};

/// Outcome of the optional GeoLite2 acquisition, recorded verbatim in the
/// report so a reader knows whether the geo path was exercised.
#[derive(Debug, Clone)]
pub enum GeoStatus {
    /// Both databases downloaded; geo resolution active.
    Enabled { city: String, asn: String },
    /// Download skipped by configuration.
    Skipped,
    /// Download attempted but failed; graceful degradation exercised.
    Failed(String),
}

impl GeoStatus {
    /// Human-readable summary for the report.
    pub fn describe(&self) -> String {
        match self {
            GeoStatus::Enabled { .. } => "enabled (GeoLite2 City + ASN downloaded)".to_string(),
            GeoStatus::Skipped => "disabled (download skipped)".to_string(),
            GeoStatus::Failed(err) => {
                format!(
                    "disabled (GeoLite2 download failed: {err}; graceful degradation exercised)"
                )
            }
        }
    }
}

/// Download the GeoLite2 databases when enabled, returning the resulting status.
pub async fn acquire_geo(config: &Config, data_dir: &Path) -> GeoStatus {
    if !config.geo_enabled {
        return GeoStatus::Skipped;
    }
    let city = data_dir.join("GeoLite2-City.mmdb");
    let asn = data_dir.join("GeoLite2-ASN.mmdb");
    match download_two(&config.city_url, &city, &config.asn_url, &asn).await {
        Ok(()) => GeoStatus::Enabled {
            city: city.to_string_lossy().into_owned(),
            asn: asn.to_string_lossy().into_owned(),
        },
        Err(err) => GeoStatus::Failed(err.to_string()),
    }
}

async fn download_two(
    city_url: &str,
    city_path: &Path,
    asn_url: &str,
    asn_path: &Path,
) -> anyhow::Result<()> {
    download(city_url, city_path).await?;
    download(asn_url, asn_path).await?;
    Ok(())
}

async fn download(url: &str, dest: &Path) -> anyhow::Result<()> {
    let bytes = reqwest::get(url).await?.error_for_status()?.bytes().await?;
    std::fs::write(dest, &bytes)?;
    Ok(())
}

/// Spawn the server, wiring env for the loopback bind address, the local data
/// directory, INFO logging, optional geo databases, and the RIS filter.
/// stdout and stderr are redirected to `log_path`.
pub fn spawn(config: &Config, geo: &GeoStatus, log_path: &Path) -> anyhow::Result<Child> {
    let log = File::create(log_path)?;
    let log_err = log.try_clone()?;

    let mut cmd = Command::new(&config.bin);
    cmd.env("CHRONOS_BIND_ADDR", BIND_ADDR)
        .env("CHRONOS_DATA_DIR", config.data_dir())
        .env(
            "RUST_LOG",
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "info,chronos_ingest=info,chronos_server=info".to_string()),
        )
        // The log is a file, not a terminal; disable ANSI color so level tokens
        // parse cleanly and the embedded startup log is readable in the report.
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .kill_on_drop(true);

    if let GeoStatus::Enabled { city, asn } = geo {
        cmd.env("CHRONOS_GEOLITE2_CITY_DB", city)
            .env("CHRONOS_GEOLITE2_ASN_DB", asn);
    }
    if let Some(host) = &config.ris_host {
        cmd.env("CHRONOS_RIS_HOST", host);
    }
    if let Some(url) = &config.ris_url {
        cmd.env("CHRONOS_RIS_URL", url);
    }

    Ok(cmd.spawn()?)
}

/// Poll `/readyz` until the server reports ready, the child exits, or the
/// timeout elapses. Returns an error only if the server never became ready.
pub async fn wait_ready(child: &mut Child, timeout: Duration) -> anyhow::Result<()> {
    let url = format!("http://{BIND_ADDR}/readyz");
    let client = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("server exited during startup with status {status}");
        }
        if let Ok(resp) = client.get(&url).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("server did not become ready within {timeout:?}");
        }
        sleep(Duration::from_millis(500)).await;
    }
}

/// Fetch and parse `/metrics`, returning an empty snapshot on any transient
/// failure so a single scrape miss never aborts the run.
pub async fn scrape_metrics(client: &reqwest::Client) -> crate::metrics::MetricsSnapshot {
    let url = format!("http://{BIND_ADDR}/metrics");
    match client.get(&url).send().await {
        Ok(resp) => match resp.text().await {
            Ok(body) => crate::metrics::MetricsSnapshot::parse(&body),
            Err(_) => crate::metrics::MetricsSnapshot::default(),
        },
        Err(_) => crate::metrics::MetricsSnapshot::default(),
    }
}
