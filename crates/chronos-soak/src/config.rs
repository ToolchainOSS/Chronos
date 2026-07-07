//! Harness configuration, resolved from positional args and environment
//! variables. Mirrors the interface of the shell scripts it replaces so
//! existing muscle memory and CI invocations keep working.
//!
//! Positional args (all optional): `duration_secs warmup_secs interval_secs`.
//! A short run doubles as the resource baseline; a long run is the production
//! soak. Env overrides are documented on each field.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

use std::env;
use std::path::PathBuf;
use std::time::Duration;

/// Fixed loopback bind address for the harness-managed server. A non-default
/// port avoids clashing with a developer's foreground instance on 8080.
pub const BIND_ADDR: &str = "127.0.0.1:8089";

/// Default GeoLite2 mirrors (a typical instance runs with geo enabled).
const DEFAULT_CITY_URL: &str = "https://s.joefang.org/GeoLite2-City";
const DEFAULT_ASN_URL: &str = "https://s.joefang.org/GeoLite2-ASN";

/// Fully resolved run configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Path to the release `chronos-server` binary under test.
    pub bin: PathBuf,
    /// Measurement window after warmup.
    pub duration: Duration,
    /// Warmup grace (CAIDA download, RIS connect, initial topology burst)
    /// excluded from the measured figures.
    pub warmup: Duration,
    /// Interval between monitor samples.
    pub interval: Duration,
    /// Output directory for the log, CSV time series, and Markdown report.
    pub out_dir: PathBuf,
    /// Destination for the Markdown report.
    pub report_path: PathBuf,
    /// Optional RIS collector host filter, passed through to the server. Cuts
    /// ingress (and CPU) substantially versus the unfiltered firehose.
    pub ris_host: Option<String>,
    /// Optional RIS Live URL override.
    pub ris_url: Option<String>,
    /// Whether to download GeoLite2 (skipped when `SOAK_SKIP_GEO=1`).
    pub geo_enabled: bool,
    /// GeoLite2 City and ASN mirror URLs.
    pub city_url: String,
    pub asn_url: String,
}

impl Config {
    /// Resolve configuration from args and environment. `repo_root` locates the
    /// default binary path relative to the workspace.
    pub fn from_args_and_env(repo_root: &std::path::Path) -> anyhow::Result<Self> {
        let mut args = env::args().skip(1);
        let duration = arg_or_env_secs(args.next(), "SOAK_DURATION", 1200)?;
        let warmup = arg_or_env_secs(args.next(), "SOAK_WARMUP", 60)?;
        let interval = arg_or_env_secs(args.next(), "SOAK_INTERVAL", 10)?;

        if interval.is_zero() {
            anyhow::bail!("sample interval must be greater than zero");
        }
        if duration < interval {
            anyhow::bail!(
                "duration ({}s) must be at least one sample interval ({}s)",
                duration.as_secs(),
                interval.as_secs()
            );
        }

        let bin = env::var_os("CHRONOS_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| repo_root.join("target/release/chronos-server"));
        if !bin.exists() {
            anyhow::bail!(
                "release binary not found at {} (run: cargo build --release --bin chronos-server)",
                bin.display()
            );
        }

        let out_dir = match env::var_os("SOAK_OUT_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => default_out_dir(),
        };
        let report_path = env::var_os("SOAK_REPORT")
            .map(PathBuf::from)
            .unwrap_or_else(|| out_dir.join("soak-report.md"));

        Ok(Self {
            bin,
            duration,
            warmup,
            interval,
            out_dir,
            report_path,
            ris_host: non_empty_env("CHRONOS_RIS_HOST"),
            ris_url: non_empty_env("CHRONOS_RIS_URL"),
            geo_enabled: env::var("SOAK_SKIP_GEO").ok().as_deref() != Some("1"),
            city_url: non_empty_env("CHRONOS_GEOLITE2_CITY_URL")
                .unwrap_or_else(|| DEFAULT_CITY_URL.to_string()),
            asn_url: non_empty_env("CHRONOS_GEOLITE2_ASN_URL")
                .unwrap_or_else(|| DEFAULT_ASN_URL.to_string()),
        })
    }

    /// Number of monitor samples over the measurement window.
    pub fn sample_count(&self) -> u64 {
        self.duration.as_secs() / self.interval.as_secs()
    }

    /// Server log path.
    pub fn log_path(&self) -> PathBuf {
        self.out_dir.join("server.log")
    }

    /// CSV time-series path.
    pub fn csv_path(&self) -> PathBuf {
        self.out_dir.join("samples.csv")
    }

    /// Local data directory handed to the server for cached datasets.
    pub fn data_dir(&self) -> PathBuf {
        self.out_dir.join("data")
    }
}

fn arg_or_env_secs(arg: Option<String>, env_key: &str, default: u64) -> anyhow::Result<Duration> {
    let raw = arg.or_else(|| env::var(env_key).ok());
    match raw {
        Some(s) => {
            let secs = s.parse::<u64>().map_err(|e| {
                anyhow::anyhow!("invalid duration '{s}' (from arg or {env_key}): {e}")
            })?;
            Ok(Duration::from_secs(secs))
        }
        None => Ok(Duration::from_secs(default)),
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    env::var(key).ok().filter(|s| !s.is_empty())
}

fn default_out_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    env::temp_dir().join(format!("chronos-soak.{nanos}"))
}
