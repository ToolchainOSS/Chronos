//! A binary radix trie mapping IP prefixes to their announcing origin ASN.
//!
//! The trie is keyed on the prefix bits (most significant bit first). IPv4 and
//! IPv6 prefixes are stored in separate trees because their bit lengths differ.
//! Each terminal node records the current origin plus a little history so the
//! detection layer can recognize when a prefix is suddenly announced by a
//! different origin (a potential hijack) and can perform longest prefix matches
//! (useful for subprefix hijack detection).
//!
//! Concurrency: the whole structure is guarded by a `parking_lot::RwLock`, which
//! provides many concurrent readers or a single atomic writer.

use chronos_types::{Asn, IpPrefix};
use ipnetwork::IpNetwork;
use parking_lot::RwLock;

/// The result of observing an announcement for a prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginObservation {
    /// The origin recorded for this exact prefix before this observation.
    pub previous_origin: Option<Asn>,
    /// The origin now recorded for this exact prefix.
    pub current_origin: Asn,
    /// True when a new distinct origin was observed for this prefix (one not seen
    /// before). Alternating between already known origins is not a change.
    pub changed: bool,
    /// The number of distinct origins seen for this exact prefix.
    pub distinct_origins: u32,
}

impl OriginObservation {
    /// True when this observation indicates a possible origin hijack: a new
    /// origin ASN appeared for a prefix that was already announced by at least
    /// one other origin.
    #[inline]
    pub fn is_possible_hijack(&self) -> bool {
        self.changed && self.previous_origin.is_some()
    }
}

#[derive(Default)]
struct Node {
    children: [Option<Box<Node>>; 2],
    entry: Option<Entry>,
}

/// An upper bound on the distinct origins tracked per prefix. A prefix seen with
/// this many origins is already flagrantly unstable; capping the set bounds
/// memory and prevents an alert storm from a pathological prefix.
const MAX_TRACKED_ORIGINS: usize = 16;

struct Entry {
    /// The most recently announced origin (what `longest_match` returns).
    origin: Asn,
    /// The distinct origins seen for this prefix, in first seen order, bounded by
    /// `MAX_TRACKED_ORIGINS`. Tracking the set (rather than just the last origin)
    /// is what stops legitimate multi origin (MOAS) prefixes from flip flopping:
    /// once an origin is known, peers alternating between known origins raise no
    /// further hijack signal.
    origins: Vec<Asn>,
}

#[derive(Default)]
struct Trie {
    root: Node,
}

impl Trie {
    fn observe(&mut self, bits: &BitKey, origin: Asn) -> OriginObservation {
        let mut node = &mut self.root;
        for i in 0..bits.len {
            let bit = bits.bit(i) as usize;
            node = node.children[bit].get_or_insert_with(|| Box::new(Node::default()));
        }

        match node.entry.as_mut() {
            Some(entry) => {
                let previous = entry.origin;
                // Always track the latest origin so longest match reflects the
                // current announcement.
                entry.origin = origin;

                // A hijack signal fires only on a genuinely new origin: one not
                // already in the known set. Alternating between known origins (a
                // stable MOAS prefix observed from many vantage points) is not a
                // change.
                let is_new = !entry.origins.contains(&origin);
                if is_new && entry.origins.len() < MAX_TRACKED_ORIGINS {
                    entry.origins.push(origin);
                }
                // Suppress once the set is saturated so a chaotic prefix does not
                // storm; it has already been flagged repeatedly.
                let changed = is_new && entry.origins.len() <= MAX_TRACKED_ORIGINS;

                OriginObservation {
                    previous_origin: Some(previous),
                    current_origin: origin,
                    changed,
                    distinct_origins: entry.origins.len() as u32,
                }
            }
            None => {
                node.entry = Some(Entry {
                    origin,
                    origins: vec![origin],
                });
                OriginObservation {
                    previous_origin: None,
                    current_origin: origin,
                    changed: false,
                    distinct_origins: 1,
                }
            }
        }
    }

    fn remove(&mut self, bits: &BitKey) -> bool {
        let mut node = &mut self.root;
        for i in 0..bits.len {
            let bit = bits.bit(i) as usize;
            match node.children[bit].as_mut() {
                Some(child) => node = child,
                None => return false,
            }
        }
        node.entry.take().is_some()
    }

    fn longest_match(&self, bits: &BitKey) -> Option<Asn> {
        let mut node = &self.root;
        let mut best: Option<Asn> = node.entry.as_ref().map(|e| e.origin);
        for i in 0..bits.len {
            let bit = bits.bit(i) as usize;
            match node.children[bit].as_ref() {
                Some(child) => {
                    node = child;
                    if let Some(entry) = node.entry.as_ref() {
                        best = Some(entry.origin);
                    }
                }
                None => break,
            }
        }
        best
    }
}

/// A concurrent prefix table (origin store) backed by two radix tries.
#[derive(Default)]
pub struct PrefixTable {
    v4: RwLock<Trie>,
    v6: RwLock<Trie>,
}

impl PrefixTable {
    /// Create an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an announcement of `prefix` by `origin`, returning what changed.
    pub fn observe(&self, prefix: &IpPrefix, origin: Asn) -> OriginObservation {
        let bits = BitKey::from_prefix(prefix);
        match prefix {
            IpPrefix::V4(_) => self.v4.write().observe(&bits, origin),
            IpPrefix::V6(_) => self.v6.write().observe(&bits, origin),
        }
    }

    /// Remove a prefix (for example on withdrawal). Returns true if it existed.
    pub fn remove(&self, prefix: &IpPrefix) -> bool {
        let bits = BitKey::from_prefix(prefix);
        match prefix {
            IpPrefix::V4(_) => self.v4.write().remove(&bits),
            IpPrefix::V6(_) => self.v6.write().remove(&bits),
        }
    }

    /// Find the origin of the most specific covering prefix for `prefix`.
    pub fn longest_match(&self, prefix: &IpPrefix) -> Option<Asn> {
        let bits = BitKey::from_prefix(prefix);
        match prefix {
            IpPrefix::V4(_) => self.v4.read().longest_match(&bits),
            IpPrefix::V6(_) => self.v6.read().longest_match(&bits),
        }
    }
}

/// A compact view of a prefix as a bit sequence (most significant bit first).
struct BitKey {
    bytes: [u8; 16],
    len: u8,
}

impl BitKey {
    fn from_prefix(prefix: &IpPrefix) -> Self {
        let net: IpNetwork = (*prefix).into();
        let mut bytes = [0u8; 16];
        let len = match net {
            IpNetwork::V4(v4) => {
                bytes[..4].copy_from_slice(&v4.network().octets());
                v4.prefix()
            }
            IpNetwork::V6(v6) => {
                bytes.copy_from_slice(&v6.network().octets());
                v6.prefix()
            }
        };
        Self { bytes, len }
    }

    #[inline]
    fn bit(&self, index: u8) -> u8 {
        let byte = self.bytes[(index / 8) as usize];
        (byte >> (7 - (index % 8))) & 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn p(s: &str) -> IpPrefix {
        IpPrefix::from_str(s).unwrap()
    }

    #[test]
    fn first_observation_is_not_a_hijack() {
        let table = PrefixTable::new();
        let obs = table.observe(&p("192.0.2.0/24"), Asn(64500));
        assert!(!obs.is_possible_hijack());
        assert_eq!(obs.previous_origin, None);
        assert_eq!(obs.distinct_origins, 1);
    }

    #[test]
    fn repeat_same_origin_is_not_a_change() {
        let table = PrefixTable::new();
        table.observe(&p("192.0.2.0/24"), Asn(64500));
        let obs = table.observe(&p("192.0.2.0/24"), Asn(64500));
        assert!(!obs.changed);
        assert_eq!(obs.distinct_origins, 1);
    }

    #[test]
    fn origin_change_flags_hijack() {
        let table = PrefixTable::new();
        table.observe(&p("192.0.2.0/24"), Asn(64500));
        let obs = table.observe(&p("192.0.2.0/24"), Asn(64510));
        assert!(obs.is_possible_hijack());
        assert_eq!(obs.previous_origin, Some(Asn(64500)));
        assert_eq!(obs.current_origin, Asn(64510));
        assert_eq!(obs.distinct_origins, 2);
    }

    #[test]
    fn moas_flip_flop_flags_once_per_new_origin() {
        // A stable multi origin prefix observed from many vantage points: the
        // origin alternates between two known ASNs. Only the first appearance of
        // the second origin is a change; subsequent flips raise no signal.
        let table = PrefixTable::new();
        let prefix = p("192.0.2.0/24");
        assert!(!table.observe(&prefix, Asn(64500)).changed); // first sighting
        assert!(table.observe(&prefix, Asn(64510)).changed); // new origin: flag
        // Alternating between the two known origins is not a change.
        for _ in 0..100 {
            assert!(!table.observe(&prefix, Asn(64500)).changed);
            assert!(!table.observe(&prefix, Asn(64510)).changed);
        }
        // A genuinely new third origin flags again and escalates the count.
        let obs = table.observe(&prefix, Asn(64520));
        assert!(obs.changed);
        assert_eq!(obs.distinct_origins, 3);
    }

    #[test]
    fn distinct_origins_is_bounded() {
        // Even a pathological prefix cycling through many origins must not grow
        // its tracked set without bound.
        let table = PrefixTable::new();
        let prefix = p("192.0.2.0/24");
        for i in 0..1000u32 {
            table.observe(&prefix, Asn(i));
        }
        let obs = table.observe(&prefix, Asn(10_000));
        assert!(obs.distinct_origins <= 16);
    }

    #[test]
    fn longest_match_prefers_more_specific() {
        let table = PrefixTable::new();
        table.observe(&p("10.0.0.0/8"), Asn(100));
        table.observe(&p("10.1.0.0/16"), Asn(200));
        assert_eq!(table.longest_match(&p("10.1.0.0/24")), Some(Asn(200)));
        assert_eq!(table.longest_match(&p("10.2.0.0/24")), Some(Asn(100)));
    }

    #[test]
    fn remove_deletes_entry() {
        let table = PrefixTable::new();
        table.observe(&p("2001:db8::/32"), Asn(64500));
        assert!(table.remove(&p("2001:db8::/32")));
        assert!(!table.remove(&p("2001:db8::/32")));
        assert_eq!(table.longest_match(&p("2001:db8::/48")), None);
    }

    #[test]
    fn v4_and_v6_are_independent() {
        let table = PrefixTable::new();
        table.observe(&p("192.0.2.0/24"), Asn(1));
        table.observe(&p("2001:db8::/32"), Asn(2));
        assert_eq!(table.longest_match(&p("192.0.2.128/25")), Some(Asn(1)));
        assert_eq!(table.longest_match(&p("2001:db8:1::/48")), Some(Asn(2)));
    }
}
