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
    /// Optional AS-path filter; a bare ASN matches any update whose path
    /// traverses that AS ("anything involving my network"). `None` means no
    /// path filter.
    pub path: Option<String>,
    /// Optional prefix filter; only updates covering this prefix are delivered.
    /// `None` means no prefix filter.
    pub prefix: Option<String>,
    /// With `prefix`, also include more-specific prefixes (catches sub-prefix
    /// hijacks). Ignored when `prefix` is `None`.
    pub more_specific: bool,
    /// With `prefix`, also include less-specific prefixes. Ignored when `prefix`
    /// is `None`.
    pub less_specific: bool,
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
            url: "ws://ris-live.ripe.net/v1/ws/?client=chronos".to_string(),
            host: None,
            path: None,
            prefix: None,
            more_specific: false,
            less_specific: false,
            require_updates_only: true,
            min_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
        }
    }
}
