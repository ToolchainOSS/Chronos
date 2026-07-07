//! Valley free path leak heuristic (blueprint Task 3.2).
//!
//! Under the Gao Rexford model, a route announcement read in propagation order
//! (from origin toward the receiver) must follow the pattern:
//!
//!   (customer to provider)* (peer to peer)? (provider to customer)*
//!
//! That is: zero or more uphill edges, then at most one peer edge, then zero or
//! more downhill edges. An AS violates this policy when it exports a route to a
//! provider or peer that it learned from a provider or peer: it is providing
//! transit for traffic that does not originate from (or terminate at) one of its
//! customers.
//!
//! We detect this locally, at each intermediate AS (the "valley apex"), rather
//! than by walking a global phase machine over the whole path. The local test
//! requires both edges incident to the apex to have a known relationship, so
//! that gaps in the relationship dataset cannot manufacture a phantom valley by
//! silently bridging two distant known edges across skipped unknown ones. This
//! trades a little recall for markedly higher precision, which is the right call
//! against a live feed where most inter AS edges are absent from the dataset.
//!
//! Detection is edge triggered per offending AS. The RIS Live feed is a per peer
//! firehose, so a single leaking announcement is observed and replayed by many
//! collector peers; inspecting each message independently would report the same
//! leak hundreds of times. We therefore suppress repeat reports for an offending
//! AS within a trailing window, so a leak episode is reported once rather than
//! once per vantage point.

use crate::anomaly::{Anomaly, Severity};
use crate::relationships::{Relationship, RelationshipProvider};
use chronos_types::Asn;
use std::collections::HashMap;

/// How long (seconds) to suppress repeat leak reports for the same offending AS.
/// A leak that keeps recurring within this window is treated as one ongoing
/// episode and reported once; a fresh report follows only after a quiet gap.
const SUPPRESS_WINDOW_SECS: f64 = 60.0;

/// Upper bound on the number of offending ASNs tracked for suppression. When
/// exceeded, entries older than the suppression window are pruned.
const RECENT_CAPACITY: usize = 8192;

/// Detects valley free violations in AS_PATHs using a relationship provider.
pub struct PathLeakDetector<P: RelationshipProvider> {
    provider: P,
    /// Last report time per offending ASN, used to collapse the per peer fan out
    /// of a single leak into one episode.
    recent: HashMap<Asn, f64>,
}

impl<P: RelationshipProvider> PathLeakDetector<P> {
    /// Create a detector backed by the given relationship provider.
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            recent: HashMap::new(),
        }
    }

    /// Inspect an AS_PATH (origin last) for a valley free violation, observed at
    /// `now_secs` (a Unix timestamp).
    ///
    /// Returns a `PathLeak` anomaly identifying the first offending ASN (the AS,
    /// nearest the origin, that exported a route creating the valley). Only
    /// apexes whose two incident edges are both known are considered, so missing
    /// relationship data never produces a false positive. A given offending AS is
    /// reported at most once per suppression window so that the many per peer
    /// copies of one leak do not each raise an anomaly.
    pub fn inspect(&mut self, path: &[Asn], now_secs: f64) -> Option<Anomaly> {
        let (offending, violations) = self.find_valley(path)?;

        // Edge trigger: suppress a repeat report while the same offending AS is
        // still within its episode window, refreshing the timestamp so an ongoing
        // leak stays quiet until it clears.
        if let Some(&last) = self.recent.get(&offending) {
            if now_secs - last < SUPPRESS_WINDOW_SECS {
                self.recent.insert(offending, now_secs);
                return None;
            }
        }
        self.remember(offending, now_secs);

        let severity = if violations > 1 {
            Severity::High
        } else {
            Severity::Medium
        };
        Some(Anomaly::PathLeak {
            path: path.to_vec(),
            offending_asn: offending,
            severity,
        })
    }

    /// Record that `offending` was reported at `now_secs`, pruning stale entries
    /// if the tracking map has grown past its capacity.
    fn remember(&mut self, offending: Asn, now_secs: f64) {
        if self.recent.len() >= RECENT_CAPACITY {
            let cutoff = now_secs - SUPPRESS_WINDOW_SECS;
            self.recent.retain(|_, &mut last| last >= cutoff);
        }
        self.recent.insert(offending, now_secs);
    }

    /// Locate the valley apex nearest the origin, returning the offending ASN and
    /// the total number of apexes that violate the valley free property. This is
    /// a pure function of the path and the relationship data.
    fn find_valley(&self, path: &[Asn]) -> Option<(Asn, u32)> {
        // A valley apex requires an AS with a neighbor on each side: at least
        // three ASNs (two edges).
        if path.len() < 3 {
            return None;
        }

        let mut violations = 0u32;
        let mut offending: Option<Asn> = None;

        // Examine each intermediate AS `y` as a potential valley apex. `path` is
        // receiver first and origin last, so for index `i` the neighbor toward
        // the origin (from which `y` learned the route) is `path[i + 1]` and the
        // neighbor toward the receiver (to which `y` re-exported it) is
        // `path[i - 1]`. Iterate nearest the origin first so the reported
        // offending ASN is the one closest to the origin.
        for i in (1..path.len() - 1).rev() {
            let y = path[i];
            let learned_from = path[i + 1];
            let exported_to = path[i - 1];

            // `relationship(a, b)` is `a`'s role relative to `b`. From `y`'s
            // point of view, a Customer role means the neighbor is `y`'s provider
            // and a Peer role means the neighbor is a peer; either way the route
            // did not come from (or go to) a customer.
            let inbound = self.provider.relationship(y, learned_from);
            let outbound = self.provider.relationship(y, exported_to);

            let from_provider_or_peer =
                matches!(inbound, Relationship::Customer | Relationship::Peer);
            let to_provider_or_peer =
                matches!(outbound, Relationship::Customer | Relationship::Peer);

            // Require both edges to be known: an Unknown edge cannot participate
            // in a confident valley judgement.
            if inbound == Relationship::Unknown || outbound == Relationship::Unknown {
                continue;
            }

            if from_provider_or_peer && to_provider_or_peer {
                violations += 1;
                offending.get_or_insert(y);
            }
        }

        offending.map(|asn| (asn, violations))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A relationship table for tests, keyed by directed (a, b) pairs.
    struct StaticProvider {
        table: HashMap<(u32, u32), Relationship>,
    }

    impl StaticProvider {
        fn new(pairs: &[(u32, u32, Relationship)]) -> Self {
            let mut table = HashMap::new();
            for &(a, b, rel) in pairs {
                table.insert((a, b), rel);
                let inverse = match rel {
                    Relationship::Provider => Relationship::Customer,
                    Relationship::Customer => Relationship::Provider,
                    other => other,
                };
                table.insert((b, a), inverse);
            }
            Self { table }
        }
    }

    impl RelationshipProvider for StaticProvider {
        fn relationship(&self, a: Asn, b: Asn) -> Relationship {
            self.table
                .get(&(a.value(), b.value()))
                .copied()
                .unwrap_or(Relationship::Unknown)
        }
    }

    #[test]
    fn valid_valley_free_path_is_clean() {
        // Path (receiver first): 4 <- 3 <- 2 <- 1 (origin).
        // Propagation order 1 -> 2 -> 3 -> 4.
        // 1 customer of 2, 2 customer of 3 (uphill), 3 provider of 4 (downhill).
        let provider = StaticProvider::new(&[
            (1, 2, Relationship::Customer),
            (2, 3, Relationship::Customer),
            (3, 4, Relationship::Provider),
        ]);
        let mut detector = PathLeakDetector::new(provider);
        let path = [Asn(4), Asn(3), Asn(2), Asn(1)];
        assert!(detector.inspect(&path, 1000.0).is_none());
    }

    #[test]
    fn valley_is_flagged() {
        // Propagation order 1 -> 2 -> 3.
        // 1 is a provider of 2 (downhill), then 2 is a customer of 3 (uphill):
        // that uphill after a downhill is a valley. Offending ASN is 2.
        let provider = StaticProvider::new(&[
            (1, 2, Relationship::Provider),
            (2, 3, Relationship::Customer),
        ]);
        let mut detector = PathLeakDetector::new(provider);
        let path = [Asn(3), Asn(2), Asn(1)];
        let anomaly = detector.inspect(&path, 1000.0).unwrap();
        match anomaly {
            Anomaly::PathLeak { offending_asn, .. } => assert_eq!(offending_asn, Asn(2)),
            _ => panic!("expected a path leak"),
        }
    }

    #[test]
    fn unknown_relationships_do_not_flag() {
        let provider = StaticProvider::new(&[]);
        let mut detector = PathLeakDetector::new(provider);
        let path = [Asn(3), Asn(2), Asn(1)];
        assert!(detector.inspect(&path, 1000.0).is_none());
    }

    #[test]
    fn short_paths_are_ignored() {
        let provider = StaticProvider::new(&[(1, 2, Relationship::Customer)]);
        let mut detector = PathLeakDetector::new(provider);
        assert!(detector.inspect(&[Asn(2), Asn(1)], 1000.0).is_none());
    }

    #[test]
    fn partial_coverage_does_not_manufacture_valley() {
        // Propagation order 1 -> 2 -> 3 -> 4 with a gap in the dataset: the
        // 2 <-> 3 edge is unknown. A global phase machine that skipped the
        // unknown edge would bridge the 1 -> 2 downhill and the 3 -> 4 uphill
        // into a phantom valley. The local apex rule refuses to judge either
        // apex because each has an unknown incident edge, so nothing is flagged.
        let provider = StaticProvider::new(&[
            (1, 2, Relationship::Provider),
            (3, 4, Relationship::Customer),
        ]);
        let mut detector = PathLeakDetector::new(provider);
        let path = [Asn(4), Asn(3), Asn(2), Asn(1)];
        assert!(detector.inspect(&path, 1000.0).is_none());
    }

    #[test]
    fn peer_transit_between_customers_is_clean() {
        // Propagation order 1 -> 2 -> 3: 1 is a customer of 2 (uphill), then
        // 2 and 3 are peers. 2 learned from a customer, so re-exporting to a
        // peer is legitimate; no valley.
        let provider =
            StaticProvider::new(&[(1, 2, Relationship::Customer), (2, 3, Relationship::Peer)]);
        let mut detector = PathLeakDetector::new(provider);
        let path = [Asn(3), Asn(2), Asn(1)];
        assert!(detector.inspect(&path, 1000.0).is_none());
    }

    #[test]
    fn multiple_adjacent_valleys_are_high_severity() {
        // Propagation order 1 -> 2 -> 3 -> 4 -> 5 where both 2 and 3 leak. 2
        // learns from its provider (1) and re-exports to its peer (3); 3 learns
        // from that peer (2) and re-exports to its provider (4). Two apexes flag,
        // so the severity is escalated and the offending ASN is the one nearest
        // the origin.
        let provider = StaticProvider::new(&[
            (1, 2, Relationship::Provider),
            (2, 3, Relationship::Peer),
            (3, 4, Relationship::Customer),
        ]);
        let mut detector = PathLeakDetector::new(provider);
        let path = [Asn(5), Asn(4), Asn(3), Asn(2), Asn(1)];
        let anomaly = detector.inspect(&path, 1000.0).unwrap();
        match anomaly {
            Anomaly::PathLeak {
                offending_asn,
                severity,
                ..
            } => {
                assert_eq!(offending_asn, Asn(2));
                assert_eq!(severity, Severity::High);
            }
            _ => panic!("expected a path leak"),
        }
    }

    #[test]
    fn repeat_leak_from_many_peers_is_reported_once() {
        // The same leaking path replayed by many peers within the suppression
        // window must raise a single anomaly, not one per vantage point.
        let provider = StaticProvider::new(&[
            (1, 2, Relationship::Provider),
            (2, 3, Relationship::Customer),
        ]);
        let mut detector = PathLeakDetector::new(provider);
        let path = [Asn(3), Asn(2), Asn(1)];

        let mut emissions = 0;
        for i in 0..500 {
            // Many observations, all within the 60 second suppression window.
            if detector.inspect(&path, 1000.0 + i as f64 * 0.05).is_some() {
                emissions += 1;
            }
        }
        assert_eq!(emissions, 1, "one leak episode should emit once");

        // After a quiet gap longer than the window, a fresh episode is reported.
        assert!(detector.inspect(&path, 1000.0 + 200.0).is_some());
    }
}
