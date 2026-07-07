//! History configuration, loaded from environment variables.
//!
//! History is **opt-in**: unless `CHRONOS_HISTORY_ENABLED` is truthy the engine
//! stays fully stateless and in-memory (its documented default). When enabled,
//! Chronos-owned storage is bounded by both an age window and a hard byte cap so
//! disk use is bounded perpetually rather than growing without limit.

use std::env;
use std::time::Duration;

/// Default hard storage cap for Chronos-owned history: 2 GiB.
const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Configuration for the persistent history subsystem.
#[derive(Debug, Clone)]
pub struct HistoryConfig {
    /// Whether history persistence is active. Off by default.
    pub enabled: bool,
    /// PostgreSQL connection string (`postgres://user:pass@host:5432/db`).
    pub url: String,
    /// Bound of the writer channel; full means drop (never block the engine).
    pub channel_bound: usize,
    /// Maximum events per `INSERT` batch.
    pub batch_size: usize,
    /// Maximum time a partially filled batch waits before being flushed.
    pub flush_interval: Duration,
    /// Interval between retention pruning passes.
    pub prune_interval: Duration,
    /// Days of high-resolution history to retain before dropping whole days.
    pub retention_days: u32,
    /// Hard ceiling on total Chronos-owned history storage, in bytes.
    pub max_bytes: u64,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            channel_bound: 8_192,
            batch_size: 500,
            flush_interval: Duration::from_secs(2),
            prune_interval: Duration::from_secs(3_600),
            retention_days: 30,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

impl HistoryConfig {
    /// Build a configuration from environment variables, falling back to
    /// defaults for anything unset. The connection string comes from
    /// `CHRONOS_HISTORY_URL`, or `DATABASE_URL` as a fallback.
    pub fn from_env() -> anyhow::Result<Self> {
        let mut config = HistoryConfig::default();

        if let Some(v) = parse_bool("CHRONOS_HISTORY_ENABLED")? {
            config.enabled = v;
        }
        config.url = env::var("CHRONOS_HISTORY_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| env::var("DATABASE_URL").ok().filter(|s| !s.is_empty()))
            .unwrap_or_default();
        if let Some(v) = parse_usize("CHRONOS_HISTORY_CHANNEL_BOUND")? {
            config.channel_bound = v;
        }
        if let Some(v) = parse_usize("CHRONOS_HISTORY_BATCH_SIZE")? {
            config.batch_size = v.max(1);
        }
        if let Some(v) = parse_u64("CHRONOS_HISTORY_FLUSH_SECS")? {
            config.flush_interval = Duration::from_secs(v.max(1));
        }
        if let Some(v) = parse_u64("CHRONOS_HISTORY_PRUNE_SECS")? {
            config.prune_interval = Duration::from_secs(v.max(1));
        }
        if let Some(v) = parse_u64("CHRONOS_HISTORY_RETENTION_DAYS")? {
            config.retention_days = v.min(u64::from(u32::MAX)) as u32;
        }
        if let Some(v) = parse_u64("CHRONOS_HISTORY_MAX_BYTES")? {
            config.max_bytes = v;
        }

        if config.enabled && config.url.is_empty() {
            anyhow::bail!(
                "CHRONOS_HISTORY_ENABLED is set but no database URL is configured; \
                 set CHRONOS_HISTORY_URL (or DATABASE_URL)"
            );
        }

        Ok(config)
    }
}

fn parse_bool(key: &str) -> anyhow::Result<Option<bool>> {
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

fn parse_usize(key: &str) -> anyhow::Result<Option<usize>> {
    match env::var(key) {
        Ok(v) => v
            .parse::<usize>()
            .map(Some)
            .map_err(|e| anyhow::anyhow!("invalid {key} '{v}': {e}")),
        Err(_) => Ok(None),
    }
}

fn parse_u64(key: &str) -> anyhow::Result<Option<u64>> {
    match env::var(key) {
        Ok(v) => v
            .parse::<u64>()
            .map(Some)
            .map_err(|e| anyhow::anyhow!("invalid {key} '{v}': {e}")),
        Err(_) => Ok(None),
    }
}
