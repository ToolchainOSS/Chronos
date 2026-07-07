//! The AS adjacency graph.
//!
//! The graph is an undirected adjacency list keyed by ASN. Edges are derived from
//! consecutive ASNs in an AS_PATH; repeated ASNs (path prepending) are collapsed
//! so a self loop is never recorded.
//!
//! Each edge also records the time it was last observed. A periodic sweep removes
//! edges that have not been seen within a time to live; those removals are what
//! drive `LinkDown` deltas (BGP does not send an explicit "peering removed"
//! signal, so link teardown is inferred from silence).

use chronos_types::Asn;
use dashmap::DashMap;
use std::collections::HashSet;

/// Describes whether observing an edge changed the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeChange {
    /// The edge already existed; only its last seen time was refreshed.
    Existing,
    /// The edge was newly added.
    Added,
}

/// Canonicalize an undirected edge so `(a, b)` and `(b, a)` share one key.
#[inline]
fn edge_key(a: u32, b: u32) -> (u32, u32) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// A concurrent AS adjacency graph.
///
/// `adjacency` maps an ASN to the set of directly peering ASNs (used for degree
/// and neighbor queries). `last_seen` maps a canonical undirected edge key to the
/// timestamp it was most recently observed (used for aging).
#[derive(Default)]
pub struct AsGraph {
    adjacency: DashMap<u32, HashSet<u32>>,
    last_seen: DashMap<(u32, u32), f64>,
}

impl AsGraph {
    /// Create an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Total number of ASNs currently in the graph.
    pub fn node_count(&self) -> usize {
        self.adjacency.len()
    }

    /// Total number of undirected edges currently in the graph.
    pub fn edge_count(&self) -> usize {
        self.last_seen.len()
    }

    /// Add or refresh an undirected edge between two ASNs at time `now_secs`.
    ///
    /// Returns `EdgeChange::Added` when the edge is new (the caller can then emit
    /// a `LinkUp` delta) and `EdgeChange::Existing` otherwise. Self loops are
    /// ignored and reported as `Existing`.
    pub fn add_edge(&self, a: Asn, b: Asn, now_secs: f64) -> EdgeChange {
        if a == b {
            return EdgeChange::Existing;
        }
        let key = edge_key(a.value(), b.value());
        let is_new = self.last_seen.insert(key, now_secs).is_none();
        if is_new {
            self.adjacency
                .entry(a.value())
                .or_default()
                .insert(b.value());
            self.adjacency
                .entry(b.value())
                .or_default()
                .insert(a.value());
            EdgeChange::Added
        } else {
            EdgeChange::Existing
        }
    }

    /// Remove an undirected edge between two ASNs.
    ///
    /// Returns true when an edge was actually removed (the caller can then emit a
    /// `LinkDown` delta).
    pub fn remove_edge(&self, a: Asn, b: Asn) -> bool {
        let key = edge_key(a.value(), b.value());
        let removed = self.last_seen.remove(&key).is_some();
        if removed {
            self.detach(a.value(), b.value());
        }
        removed
    }

    fn detach(&self, a: u32, b: u32) {
        if let Some(mut peers) = self.adjacency.get_mut(&a) {
            peers.remove(&b);
        }
        if let Some(mut peers) = self.adjacency.get_mut(&b) {
            peers.remove(&a);
        }
    }

    /// Ingest an AS_PATH observed at `now_secs`, adding or refreshing edges for
    /// each adjacent (distinct) pair.
    ///
    /// Returns the list of newly created edges as `(a, b)` ASN pairs so the caller
    /// can translate them into `LinkUp` deltas.
    pub fn observe_path(&self, path: &[Asn], now_secs: f64) -> Vec<(Asn, Asn)> {
        let mut new_edges = Vec::new();
        let mut previous: Option<Asn> = None;
        for &asn in path {
            if let Some(prev) = previous {
                if prev != asn && self.add_edge(prev, asn, now_secs) == EdgeChange::Added {
                    new_edges.push((prev, asn));
                }
            }
            previous = Some(asn);
        }
        new_edges
    }

    /// Remove every edge whose last observation is older than `now_secs - ttl`.
    ///
    /// Returns the removed edges so the caller can emit `LinkDown` deltas.
    pub fn sweep_expired(&self, now_secs: f64, ttl_secs: f64) -> Vec<(Asn, Asn)> {
        let cutoff = now_secs - ttl_secs;
        let expired: Vec<(u32, u32)> = self
            .last_seen
            .iter()
            .filter(|entry| *entry.value() < cutoff)
            .map(|entry| *entry.key())
            .collect();

        let mut removed = Vec::with_capacity(expired.len());
        for key in expired {
            if self.last_seen.remove(&key).is_some() {
                self.detach(key.0, key.1);
                removed.push((Asn(key.0), Asn(key.1)));
            }
        }
        removed
    }

    /// Return the set of peers for an ASN, if known.
    pub fn peers(&self, asn: Asn) -> Option<HashSet<u32>> {
        self.adjacency.get(&asn.value()).map(|set| set.clone())
    }

    /// Return the peering degree (number of neighbors) of an ASN.
    pub fn degree(&self, asn: Asn) -> usize {
        self.adjacency
            .get(&asn.value())
            .map(|set| set.len())
            .unwrap_or(0)
    }

    /// Return up to `max` current edges, used to send a bounded initial snapshot
    /// to a newly connected client.
    pub fn snapshot_edges(&self, max: usize) -> Vec<(Asn, Asn)> {
        self.last_seen
            .iter()
            .take(max)
            .map(|entry| {
                let (a, b) = *entry.key();
                (Asn(a), Asn(b))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_edges_once() {
        let g = AsGraph::new();
        assert_eq!(g.add_edge(Asn(1), Asn(2), 0.0), EdgeChange::Added);
        assert_eq!(g.add_edge(Asn(1), Asn(2), 1.0), EdgeChange::Existing);
        assert_eq!(g.add_edge(Asn(2), Asn(1), 2.0), EdgeChange::Existing);
        assert_eq!(g.edge_count(), 1);
    }

    #[test]
    fn ignores_self_loops() {
        let g = AsGraph::new();
        assert_eq!(g.add_edge(Asn(5), Asn(5), 0.0), EdgeChange::Existing);
        assert_eq!(g.degree(Asn(5)), 0);
    }

    #[test]
    fn observe_path_collapses_prepends() {
        let g = AsGraph::new();
        let path = [Asn(1), Asn(1), Asn(2), Asn(3), Asn(3)];
        let new_edges = g.observe_path(&path, 0.0);
        assert_eq!(new_edges, vec![(Asn(1), Asn(2)), (Asn(2), Asn(3))]);
        // Re-observing yields no new edges (last seen is refreshed instead).
        assert!(g.observe_path(&path, 1.0).is_empty());
    }

    #[test]
    fn remove_edge_reports_removal() {
        let g = AsGraph::new();
        g.add_edge(Asn(1), Asn(2), 0.0);
        assert!(g.remove_edge(Asn(1), Asn(2)));
        assert!(!g.remove_edge(Asn(1), Asn(2)));
        assert_eq!(g.degree(Asn(1)), 0);
    }

    #[test]
    fn sweep_removes_stale_edges() {
        let g = AsGraph::new();
        g.add_edge(Asn(1), Asn(2), 0.0);
        g.add_edge(Asn(2), Asn(3), 100.0);
        // At t=110 with a ttl of 30, only the first edge is stale.
        let removed = g.sweep_expired(110.0, 30.0);
        assert_eq!(removed, vec![(Asn(1), Asn(2))]);
        assert_eq!(g.edge_count(), 1);
        assert_eq!(g.degree(Asn(2)), 1);
    }

    #[test]
    fn tracks_degree_and_peers() {
        let g = AsGraph::new();
        g.observe_path(&[Asn(10), Asn(20), Asn(30)], 0.0);
        assert_eq!(g.degree(Asn(20)), 2);
        let peers = g.peers(Asn(20)).unwrap();
        assert!(peers.contains(&10));
        assert!(peers.contains(&30));
    }
}
