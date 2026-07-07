//! AS relationship providers used by the path leak heuristic.
//!
//! Two implementations are provided:
//! - `CaidaRelationships`: parsed from a CAIDA AS relationship dataset file. The
//!   dataset is mounted into the container at runtime and its path is supplied by
//!   an environment variable (see the server configuration and the README); it is
//!   never committed to source control.
//! - `DegreeHeuristic`: a fallback used when no CAIDA dataset is configured. It
//!   infers a provider/customer relationship from peering degree in the live AS
//!   graph (a much higher degree ASN is treated as the provider). This is a rough
//!   approximation and is documented as such; it exists so the engine still
//!   produces useful signal without the mounted dataset.

use chronos_topology::AsGraph;
use chronos_types::Asn;
use std::collections::HashMap;
use std::sync::Arc;

/// The relationship between two ASNs, expressed from the perspective of the first
/// ASN passed to `RelationshipProvider::relationship`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relationship {
    /// The first ASN is a provider of the second (the second is its customer).
    Provider,
    /// The first ASN is a customer of the second (the second is its provider).
    Customer,
    /// The two ASNs are settlement free peers.
    Peer,
    /// The relationship is unknown.
    Unknown,
}

impl Relationship {
    fn inverse(self) -> Self {
        match self {
            Relationship::Provider => Relationship::Customer,
            Relationship::Customer => Relationship::Provider,
            Relationship::Peer => Relationship::Peer,
            Relationship::Unknown => Relationship::Unknown,
        }
    }
}

/// A source of pairwise AS relationships.
pub trait RelationshipProvider: Send + Sync {
    /// Return the relationship of `a` to `b`.
    fn relationship(&self, a: Asn, b: Asn) -> Relationship;
}

impl RelationshipProvider for Arc<dyn RelationshipProvider> {
    fn relationship(&self, a: Asn, b: Asn) -> Relationship {
        (**self).relationship(a, b)
    }
}

/// Relationships parsed from a CAIDA AS relationship dataset.
///
/// The CAIDA `as-rel` format uses lines of the form `as1|as2|rel` where `rel` is
/// `-1` (as1 is a provider of as2) or `0` (as1 and as2 are peers). Comment lines
/// begin with `#`.
#[derive(Debug, Default, Clone)]
pub struct CaidaRelationships {
    /// Keyed by the ordered pair (as1, as2) exactly as parsed; the stored value is
    /// the relationship of `as1` to `as2`.
    edges: HashMap<(u32, u32), Relationship>,
}

impl CaidaRelationships {
    /// Number of directed relationship entries stored.
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    /// True when no relationships were loaded.
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }
}

impl RelationshipProvider for CaidaRelationships {
    fn relationship(&self, a: Asn, b: Asn) -> Relationship {
        if let Some(rel) = self.edges.get(&(a.value(), b.value())) {
            return *rel;
        }
        if let Some(rel) = self.edges.get(&(b.value(), a.value())) {
            return rel.inverse();
        }
        Relationship::Unknown
    }
}

/// Parse a CAIDA AS relationship dataset from its textual contents.
pub fn parse_caida_as_rel(contents: &str) -> CaidaRelationships {
    let mut edges = HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('|');
        let (Some(a), Some(b), Some(rel)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) else {
            continue;
        };
        let relationship = match rel.trim() {
            // as1 is a provider of as2.
            "-1" => Relationship::Provider,
            // as1 and as2 are peers.
            "0" => Relationship::Peer,
            _ => continue,
        };
        edges.insert((a, b), relationship);
    }
    CaidaRelationships { edges }
}

/// A degree based relationship heuristic backed by the live AS graph.
///
/// When one ASN has a peering degree at least `ratio` times larger than the
/// other, it is treated as the provider. Otherwise the two are treated as peers.
/// This is an approximation used only when no CAIDA dataset is mounted.
pub struct DegreeHeuristic {
    graph: Arc<AsGraph>,
    ratio: f64,
}

impl DegreeHeuristic {
    /// Create a heuristic provider over the given graph with the given degree
    /// ratio threshold (a value of 4.0 is a reasonable default).
    pub fn new(graph: Arc<AsGraph>, ratio: f64) -> Self {
        Self { graph, ratio }
    }
}

impl RelationshipProvider for DegreeHeuristic {
    fn relationship(&self, a: Asn, b: Asn) -> Relationship {
        let da = self.graph.degree(a) as f64;
        let db = self.graph.degree(b) as f64;
        if da <= 0.0 || db <= 0.0 {
            return Relationship::Unknown;
        }
        if da >= db * self.ratio {
            Relationship::Provider
        } else if db >= da * self.ratio {
            Relationship::Customer
        } else {
            Relationship::Peer
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_caida_lines_and_comments() {
        let data = "# comment\n1|2|-1\n3|4|0\nbad line\n5|6|1\n";
        let rels = parse_caida_as_rel(data);
        assert_eq!(rels.len(), 2);
        assert_eq!(rels.relationship(Asn(1), Asn(2)), Relationship::Provider);
        // Inverse lookup returns the mirrored relationship.
        assert_eq!(rels.relationship(Asn(2), Asn(1)), Relationship::Customer);
        assert_eq!(rels.relationship(Asn(3), Asn(4)), Relationship::Peer);
        assert_eq!(rels.relationship(Asn(9), Asn(10)), Relationship::Unknown);
    }

    #[test]
    fn degree_heuristic_infers_provider() {
        let graph = Arc::new(AsGraph::new());
        // ASN 1 becomes a hub (high degree); ASN 2 is a leaf.
        for peer in 10..30 {
            graph.add_edge(Asn(1), Asn(peer), 0.0);
        }
        graph.add_edge(Asn(1), Asn(2), 0.0);
        let heuristic = DegreeHeuristic::new(graph, 4.0);
        assert_eq!(
            heuristic.relationship(Asn(1), Asn(2)),
            Relationship::Provider
        );
        assert_eq!(
            heuristic.relationship(Asn(2), Asn(1)),
            Relationship::Customer
        );
    }
}
