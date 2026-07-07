//! The persisted history event: a compact, database-agnostic record of one
//! detected routing anomaly.
//!
//! `HistoryEvent` is a plain data-transfer type with no dependency on the
//! detection or topology crates. Callers (the server pipeline) translate their
//! domain `Anomaly` into this shape, formatting prefixes as CIDR text and ASNs
//! as `i64`, so the storage layer stays decoupled from the rest of Chronos.

/// The kind of anomaly recorded. The string form matches the Prometheus metric
/// label (`prefix_hijack` / `path_leak` / `route_churn`) so history and metrics
/// use one vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A prefix announced by an unexpected origin ASN.
    PrefixHijack,
    /// An AS_PATH that violates valley-free routing policy.
    PathLeak,
    /// A prefix whose update velocity crossed the surge threshold.
    RouteChurn,
}

impl EventKind {
    /// The stable string representation stored in the database and used as the
    /// metric label.
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::PrefixHijack => "prefix_hijack",
            EventKind::PathLeak => "path_leak",
            EventKind::RouteChurn => "route_churn",
        }
    }
}

/// A single anomaly occurrence, ready to be persisted.
///
/// Fields that do not apply to a given kind are `None` (for example a
/// `PathLeak` carries an `as_path` and `offending_asn` but no `prefix`).
/// `severity` is a small ordinal (`0` = Low, `1` = Medium, `2` = High) so that
/// range queries like "at least Medium" are index-friendly.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEvent {
    /// Observation time as Unix epoch seconds (the collector timestamp when
    /// available, otherwise wall clock). Drives the daily partition it lands in.
    pub observed_at: f64,
    /// The anomaly kind.
    pub kind: EventKind,
    /// Ordinal severity: 0 = Low, 1 = Medium, 2 = High.
    pub severity: i16,
    /// The affected prefix as CIDR text (for hijack and churn events).
    pub prefix: Option<String>,
    /// The origin previously recorded for the prefix (hijack events).
    pub previous_origin: Option<i64>,
    /// The new, suspicious origin (hijack events).
    pub new_origin: Option<i64>,
    /// The ASN at which a valley was detected (path-leak events).
    pub offending_asn: Option<i64>,
    /// The full AS_PATH under inspection, origin last (path-leak events).
    pub as_path: Option<Vec<i64>>,
    /// The impacted geographic region (ISO country or subdivision), when the
    /// prefix resolved to one.
    pub region: Option<String>,
    /// Update count within the surge window (churn events).
    pub updates_in_window: Option<i32>,
    /// The adaptive threshold that was exceeded (churn events).
    pub threshold: Option<f64>,
}

impl HistoryEvent {
    /// Construct a bare event with only the required fields set; optional fields
    /// start empty and are filled in by the caller as applicable.
    pub fn new(observed_at: f64, kind: EventKind, severity: i16) -> Self {
        Self {
            observed_at,
            kind,
            severity,
            prefix: None,
            previous_origin: None,
            new_origin: None,
            offending_asn: None,
            as_path: None,
            region: None,
            updates_in_window: None,
            threshold: None,
        }
    }
}
