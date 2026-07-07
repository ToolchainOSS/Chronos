//! Egress delta frames sent to browser clients.
//!
//! The server never ships the full topology graph over the wire; it emits minimal
//! deltas that the frontend applies incrementally.

use crate::asn::Asn;
use serde::{Deserialize, Serialize};

/// A minimal change frame broadcast to connected clients.
///
/// The wire form is an internally tagged JSON object, for example:
/// `{ "kind": "LinkUp", "a": 1234, "b": 5678 }` or
/// `{ "kind": "AreaDegraded", "region": "US-CA", "severity": 0.87 }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Delta {
    /// A peering link between two ASNs became active.
    LinkUp {
        /// The first endpoint ASN.
        a: u32,
        /// The second endpoint ASN.
        b: u32,
    },
    /// A peering link between two ASNs was withdrawn or aged out.
    LinkDown {
        /// The first endpoint ASN.
        a: u32,
        /// The second endpoint ASN.
        b: u32,
    },
    /// A geographic area is degraded; the region is an ISO country or subdivision
    /// code and the severity is a normalized index in the range 0.0 to 1.0.
    AreaDegraded {
        /// ISO country or subdivision code (for example `US` or `US-CA`).
        region: String,
        /// Normalized severity index (0.0 to 1.0).
        severity: f32,
    },
}

impl Delta {
    /// Construct a `LinkUp` delta from two ASNs.
    #[inline]
    pub fn link_up(a: Asn, b: Asn) -> Self {
        Delta::LinkUp {
            a: a.value(),
            b: b.value(),
        }
    }

    /// Construct a `LinkDown` delta from two ASNs.
    #[inline]
    pub fn link_down(a: Asn, b: Asn) -> Self {
        Delta::LinkDown {
            a: a.value(),
            b: b.value(),
        }
    }

    /// Construct an `AreaDegraded` delta from a region code and severity.
    #[inline]
    pub fn area_degraded(region: impl Into<String>, severity: f32) -> Self {
        Delta::AreaDegraded {
            region: region.into(),
            severity: severity.clamp(0.0, 1.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_frames_round_trip() {
        let up = Delta::link_up(Asn(1234), Asn(5678));
        let json = serde_json::to_string(&up).unwrap();
        assert_eq!(json, r#"{"kind":"LinkUp","a":1234,"b":5678}"#);
        let back: Delta = serde_json::from_str(&json).unwrap();
        assert_eq!(up, back);
    }

    #[test]
    fn area_severity_is_clamped() {
        let d = Delta::area_degraded("US-CA", 5.0);
        match d {
            Delta::AreaDegraded { severity, .. } => assert_eq!(severity, 1.0),
            _ => panic!("wrong variant"),
        }
    }
}
