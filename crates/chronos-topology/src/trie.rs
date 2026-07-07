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
    /// True when the origin changed from a previously recorded (different) value.
    pub changed: bool,
    /// The number of distinct origins ever seen for this exact prefix.
    pub distinct_origins: u32,
}

impl OriginObservation {
    /// True when this observation indicates a possible origin hijack: the prefix
    /// was previously announced by a different ASN.
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

struct Entry {
    origin: Asn,
    distinct_origins: u32,
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
                let changed = previous != origin;
                if changed {
                    entry.origin = origin;
                    entry.distinct_origins = entry.distinct_origins.saturating_add(1);
                }
                OriginObservation {
                    previous_origin: Some(previous),
                    current_origin: origin,
                    changed,
                    distinct_origins: entry.distinct_origins,
                }
            }
            None => {
                node.entry = Some(Entry {
                    origin,
                    distinct_origins: 1,
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
