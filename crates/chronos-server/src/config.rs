//! Server configuration, loaded from environment variables with sensible
//! defaults.
//!
//! Runtime state lives under a single writable data directory
//! (`CHRONOS_DATA_DIR`, default `/data`): downloaded and cached datasets and any
//! other volatile files go there, so a deployment mounts one host path for all
//! persistent-but-derived state.
//!
//! The CAIDA AS-relationship dataset is acquired automatically in the happy
//! path: nothing needs configuring. Operators may override the source in order
//! of precedence:
//! - `CHRONOS_CAIDA_ASREL`: path to a mounted dataset file (`.txt` or `.bz2`).
//! - `CHRONOS_CAIDA_URL`: an exact `.bz2` (or `.txt`) URL to fetch and cache.
//! - Otherwise, when `CHRONOS_CAIDA_AUTODOWNLOAD` is enabled (the default), the
//!   latest dataset is discovered under `CHRONOS_CAIDA_BASE_URL` and cached.
//!
//! GeoLite2 databases (`CHRONOS_GEOLITE2_CITY_DB`, `CHRONOS_GEOLITE2_ASN_DB`)
//! remain mounted files located by path. When any dataset is unavailable the
//! corresponding feature degrades gracefully: geo resolution is skipped, and
//! path leak detection falls back to a degree based heuristic. None of these
//! files are ever bundled into the image or committed.

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Default writable data directory for cached datasets and volatile runtime
/// state. In containers this is the mounted volume; see the Dockerfile.
const DEFAULT_DATA_DIR: &str = "/data";

/// Default CAIDA directory index used to discover the latest `as-rel` dataset.
/// serial-1 carries the classic `YYYYMMDD.as-rel.txt.bz2` files.
const DEFAULT_CAIDA_BASE_URL: &str =
    "https://publicdata.caida.org/datasets/as-relationships/serial-1/";

/// Top level server configuration.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Address the HTTP and WebSocket server binds to.
    pub bind_addr: SocketAddr,
    /// RIS Live WebSocket URL.
    pub ris_url: String,
    /// Optional RIS collector host filter.
    pub ris_host: Option<String>,
    /// Bound of the ingestion channel (producer to consumer backpressure).
    pub ingest_channel_bound: usize,
    /// Capacity of the broadcast ring buffer shared with WebSocket clients.
    pub broadcast_capacity: usize,
    /// Maximum edges included in the initial snapshot sent to a new client.
    pub snapshot_max: usize,
    /// Time to live for an AS edge before it is aged out (drives LinkDown).
    pub edge_ttl: Duration,
    /// Interval between edge aging sweeps.
    pub sweep_interval: Duration,
    /// Degree ratio used by the fallback relationship heuristic.
    pub degree_ratio: f64,
    /// Writable directory for cached datasets and other volatile runtime state.
    pub data_dir: PathBuf,
    /// Path to the mounted GeoLite2 City database, if configured.
    pub geolite2_city_db: Option<PathBuf>,
    /// Path to the mounted GeoLite2 ASN database, if configured.
    pub geolite2_asn_db: Option<PathBuf>,
    /// Path to an explicitly mounted CAIDA AS relationship dataset, if set. Takes
    /// precedence over both the pinned URL and auto-download.
    pub caida_as_rel: Option<PathBuf>,
    /// Exact CAIDA dataset URL to fetch and cache, if set. Used when no mounted
    /// file is configured; takes precedence over auto-discovery.
    pub caida_url: Option<String>,
    /// Whether to auto-discover and download the latest CAIDA dataset when no
    /// mounted file or pinned URL is configured.
    pub caida_autodownload: bool,
    /// Directory index under which the latest CAIDA dataset is discovered.
    pub caida_base_url: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:8080".parse().expect("valid default bind address"),
            ris_url: "ws://ris-live.ripe.net/v1/ws/?client=chronos".to_string(),
            ris_host: None,
            ingest_channel_bound: 16_384,
            broadcast_capacity: 8_192,
            snapshot_max: 2_000,
            edge_ttl: Duration::from_secs(900),
            sweep_interval: Duration::from_secs(60),
            degree_ratio: 4.0,
            data_dir: PathBuf::from(DEFAULT_DATA_DIR),
            geolite2_city_db: None,
            geolite2_asn_db: None,
            caida_as_rel: None,
            caida_url: None,
            caida_autodownload: true,
            caida_base_url: DEFAULT_CAIDA_BASE_URL.to_string(),
        }
    }
}

impl AppConfig {
    /// Build a configuration from environment variables, falling back to
    /// defaults for anything unset.
    pub fn from_env() -> anyhow::Result<Self> {
        let mut config = AppConfig::default();

        if let Ok(addr) = env::var("CHRONOS_BIND_ADDR") {
            config.bind_addr = addr
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid CHRONOS_BIND_ADDR '{addr}': {e}"))?;
        }
        if let Ok(url) = env::var("CHRONOS_RIS_URL") {
            config.ris_url = url;
        }
        config.ris_host = env::var("CHRONOS_RIS_HOST").ok().filter(|s| !s.is_empty());

        if let Some(v) = parse_env_usize("CHRONOS_INGEST_CHANNEL_BOUND")? {
            config.ingest_channel_bound = v;
        }
        if let Some(v) = parse_env_usize("CHRONOS_BROADCAST_CAPACITY")? {
            config.broadcast_capacity = v;
        }
        if let Some(v) = parse_env_usize("CHRONOS_SNAPSHOT_MAX")? {
            config.snapshot_max = v;
        }
        if let Some(v) = parse_env_u64("CHRONOS_EDGE_TTL_SECS")? {
            config.edge_ttl = Duration::from_secs(v);
        }
        if let Some(v) = parse_env_u64("CHRONOS_SWEEP_INTERVAL_SECS")? {
            config.sweep_interval = Duration::from_secs(v);
        }
        if let Some(v) = parse_env_f64("CHRONOS_DEGREE_RATIO")? {
            config.degree_ratio = v;
        }

        if let Some(dir) = env_path("CHRONOS_DATA_DIR") {
            config.data_dir = dir;
        }

        config.geolite2_city_db = env_path("CHRONOS_GEOLITE2_CITY_DB");
        config.geolite2_asn_db = env_path("CHRONOS_GEOLITE2_ASN_DB");
        config.caida_as_rel = env_path("CHRONOS_CAIDA_ASREL");
        config.caida_url = env::var("CHRONOS_CAIDA_URL").ok().filter(|s| !s.is_empty());
        if let Some(v) = parse_env_bool("CHRONOS_CAIDA_AUTODOWNLOAD")? {
            config.caida_autodownload = v;
        }
        if let Ok(url) = env::var("CHRONOS_CAIDA_BASE_URL") {
            if !url.is_empty() {
                config.caida_base_url = url;
            }
        }

        Ok(config)
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn parse_env_usize(key: &str) -> anyhow::Result<Option<usize>> {
    match env::var(key) {
        Ok(v) => v
            .parse::<usize>()
            .map(Some)
            .map_err(|e| anyhow::anyhow!("invalid {key} '{v}': {e}")),
        Err(_) => Ok(None),
    }
}

fn parse_env_u64(key: &str) -> anyhow::Result<Option<u64>> {
    match env::var(key) {
        Ok(v) => v
            .parse::<u64>()
            .map(Some)
            .map_err(|e| anyhow::anyhow!("invalid {key} '{v}': {e}")),
        Err(_) => Ok(None),
    }
}

fn parse_env_f64(key: &str) -> anyhow::Result<Option<f64>> {
    match env::var(key) {
        Ok(v) => v
            .parse::<f64>()
            .map(Some)
            .map_err(|e| anyhow::anyhow!("invalid {key} '{v}': {e}")),
        Err(_) => Ok(None),
    }
}

fn parse_env_bool(key: &str) -> anyhow::Result<Option<bool>> {
    match env::var(key) {
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(Some(true)),
            "0" | "false" | "no" | "off" => Ok(Some(false)),
            other => Err(anyhow::anyhow!(
                "invalid {key} '{other}': expected a boolean (true/false)"
            )),
        },
        Err(_) => Ok(None),
    }
}
