//! Ingestion configuration.

use std::time::Duration;

/// Configuration for the RIS Live ingestion client.
#[derive(Debug, Clone)]
pub struct IngestConfig {
    /// The RIS Live WebSocket endpoint.
    pub url: String,
    /// Optional `host` filter (a specific collector, for example `rrc00`); when
    /// `None` the subscription covers all collectors.
    pub host: Option<String>,
    /// Whether to request that RIS Live include less common attributes; kept
    /// false by default to minimize frame size on the hot path.
    pub require_updates_only: bool,
    /// Minimum reconnect backoff.
    pub min_backoff: Duration,
    /// Maximum reconnect backoff.
    pub max_backoff: Duration,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            url: "ws://ris-live.ripe.net/v1/stream/?client=chronos".to_string(),
            host: None,
            require_updates_only: true,
            min_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
        }
    }
}
