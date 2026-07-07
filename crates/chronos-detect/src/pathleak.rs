//! Valley free path leak heuristic (blueprint Task 3.2).
//!
//! Under the Gao Rexford model, a route announcement read in propagation order
//! (from origin toward the receiver) must follow the pattern:
//!
//!   (customer to provider)* (peer to peer)? (provider to customer)*
//!
//! That is: zero or more uphill edges, then at most one peer edge, then zero or
//! more downhill edges. Any edge that goes back uphill after a peer or downhill
//! edge (a valley), or a second peer edge, is a valley free violation and a
//! possible route leak.
//!
//! Edges whose relationship is unknown are skipped so that missing relationship
//! data does not produce false positives.

use crate::anomaly::{Anomaly, Severity};
use crate::relationships::{Relationship, RelationshipProvider};
use chronos_types::Asn;

/// Detects valley free violations in AS_PATHs using a relationship provider.
pub struct PathLeakDetector<P: RelationshipProvider> {
    provider: P,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Uphill,
    Peered,
    Downhill,
}

impl<P: RelationshipProvider> PathLeakDetector<P> {
    /// Create a detector backed by the given relationship provider.
    pub fn new(provider: P) -> Self {
        Self { provider }
    }

    /// Inspect an AS_PATH (origin last) for a valley free violation.
    ///
    /// Returns a `PathLeak` anomaly identifying the first offending ASN (the ASN
    /// that exported a route in a way that creates the valley).
    pub fn inspect(&self, path: &[Asn]) -> Option<Anomaly> {
        // A valley requires at least two edges (three ASNs).
        if path.len() < 3 {
            return None;
        }

        let mut phase = Phase::Uphill;
        let mut violations = 0u32;
        let mut offending: Option<Asn> = None;

        // Iterate edges in propagation order: from origin (path end) outward.
        // `x` is closer to the origin; `y` is the next hop that received the route.
        for window in path.windows(2).rev() {
            let y = window[0];
            let x = window[1];
            let rel = self.provider.relationship(x, y);
            match rel {
                Relationship::Unknown => continue,
                Relationship::Customer => {
                    // Uphill edge (x is a customer of y).
                    if phase != Phase::Uphill {
                        violations += 1;
                        offending.get_or_insert(x);
                    }
                }
                Relationship::Peer => {
                    if phase == Phase::Uphill {
                        phase = Phase::Peered;
                    } else {
                        // A second peer edge, or a peer edge while descending.
                        violations += 1;
                        offending.get_or_insert(x);
                    }
                }
                Relationship::Provider => {
                    // Downhill edge (x is a provider of y).
                    phase = Phase::Downhill;
                }
            }
        }

        let offending = offending?;
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
        let detector = PathLeakDetector::new(provider);
        let path = [Asn(4), Asn(3), Asn(2), Asn(1)];
        assert!(detector.inspect(&path).is_none());
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
        let detector = PathLeakDetector::new(provider);
        let path = [Asn(3), Asn(2), Asn(1)];
        let anomaly = detector.inspect(&path).unwrap();
        match anomaly {
            Anomaly::PathLeak { offending_asn, .. } => assert_eq!(offending_asn, Asn(2)),
            _ => panic!("expected a path leak"),
        }
    }

    #[test]
    fn unknown_relationships_do_not_flag() {
        let provider = StaticProvider::new(&[]);
        let detector = PathLeakDetector::new(provider);
        let path = [Asn(3), Asn(2), Asn(1)];
        assert!(detector.inspect(&path).is_none());
    }

    #[test]
    fn short_paths_are_ignored() {
        let provider = StaticProvider::new(&[(1, 2, Relationship::Customer)]);
        let detector = PathLeakDetector::new(provider);
        assert!(detector.inspect(&[Asn(2), Asn(1)]).is_none());
    }
}
