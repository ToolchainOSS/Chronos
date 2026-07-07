//! The anomaly value type produced by the heuristics.

use chronos_types::{Asn, IpPrefix};

/// A normalized severity indicator for a detected anomaly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Informational; likely benign or expected (for example a known MOAS).
    Low,
    /// Noteworthy; warrants attention.
    Medium,
    /// Strong signal of a routing security or stability event.
    High,
}

impl Severity {
    /// Map the severity onto a normalized index in the range 0.0 to 1.0, suitable
    /// for the `AreaDegraded` delta severity field.
    pub fn as_index(self) -> f32 {
        match self {
            Severity::Low => 0.33,
            Severity::Medium => 0.66,
            Severity::High => 1.0,
        }
    }
}

/// A detected routing anomaly.
#[derive(Debug, Clone, PartialEq)]
pub enum Anomaly {
    /// A prefix is suddenly announced by an origin ASN different from the one
    /// historically recorded (blueprint Task 3.1).
    PrefixHijack {
        /// The affected prefix.
        prefix: IpPrefix,
        /// The origin previously recorded for the prefix.
        previous_origin: Asn,
        /// The new (suspicious) origin.
        new_origin: Asn,
        /// Severity of the observation.
        severity: Severity,
    },
    /// An AS_PATH that violates valley free routing policy (blueprint Task 3.2).
    PathLeak {
        /// The full AS_PATH under inspection (origin last).
        path: Vec<Asn>,
        /// The ASN at which the valley (an illegal uphill after a downhill or
        /// peer edge) was detected.
        offending_asn: Asn,
        /// Severity of the observation.
        severity: Severity,
    },
    /// A prefix whose update velocity crossed the MAD threshold within the
    /// sliding window (blueprint Task 3.3).
    RouteChurn {
        /// The affected prefix.
        prefix: IpPrefix,
        /// The number of updates counted within the window.
        updates_in_window: u32,
        /// The adaptive threshold that was exceeded.
        threshold: f64,
        /// Severity of the observation.
        severity: Severity,
    },
}

impl Anomaly {
    /// Return the severity of this anomaly.
    pub fn severity(&self) -> Severity {
        match self {
            Anomaly::PrefixHijack { severity, .. } => *severity,
            Anomaly::PathLeak { severity, .. } => *severity,
            Anomaly::RouteChurn { severity, .. } => *severity,
        }
    }

    /// A representative prefix for this anomaly, when one applies (used to map the
    /// event to a geographic region).
    pub fn prefix(&self) -> Option<IpPrefix> {
        match self {
            Anomaly::PrefixHijack { prefix, .. } => Some(*prefix),
            Anomaly::RouteChurn { prefix, .. } => Some(*prefix),
            Anomaly::PathLeak { .. } => None,
        }
    }
}
