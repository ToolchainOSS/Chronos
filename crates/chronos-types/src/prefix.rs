//! Compact IP prefix representation.

use ipnetwork::{IpNetwork, Ipv4Network, Ipv6Network};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// A routed IP prefix.
///
/// A custom enum (rather than a boxed or string form) keeps every prefix fixed
/// size and stack allocated during stream ingestion: the v4 variant stores four
/// address bytes plus a prefix length, the v6 variant stores sixteen bytes plus a
/// prefix length. This eliminates per prefix heap allocations on the hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IpPrefix {
    /// An IPv4 network (address plus prefix length).
    V4(Ipv4Network),
    /// An IPv6 network (address plus prefix length).
    V6(Ipv6Network),
}

/// Error returned when a prefix string cannot be parsed.
#[derive(Debug, thiserror::Error)]
#[error("invalid IP prefix '{input}': {source}")]
pub struct PrefixParseError {
    input: String,
    #[source]
    source: ipnetwork::IpNetworkError,
}

impl IpPrefix {
    /// Return the prefix length in bits (for example, 24 for a `/24`).
    #[inline]
    pub fn prefix_len(&self) -> u8 {
        match self {
            IpPrefix::V4(net) => net.prefix(),
            IpPrefix::V6(net) => net.prefix(),
        }
    }

    /// Return true when this is an IPv4 prefix.
    #[inline]
    pub fn is_ipv4(&self) -> bool {
        matches!(self, IpPrefix::V4(_))
    }

    /// Return true when this is an IPv6 prefix.
    #[inline]
    pub fn is_ipv6(&self) -> bool {
        matches!(self, IpPrefix::V6(_))
    }
}

impl From<IpNetwork> for IpPrefix {
    #[inline]
    fn from(net: IpNetwork) -> Self {
        match net {
            IpNetwork::V4(v4) => IpPrefix::V4(v4),
            IpNetwork::V6(v6) => IpPrefix::V6(v6),
        }
    }
}

impl From<IpPrefix> for IpNetwork {
    #[inline]
    fn from(prefix: IpPrefix) -> Self {
        match prefix {
            IpPrefix::V4(v4) => IpNetwork::V4(v4),
            IpPrefix::V6(v6) => IpNetwork::V6(v6),
        }
    }
}

impl FromStr for IpPrefix {
    type Err = PrefixParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        IpNetwork::from_str(s)
            .map(IpPrefix::from)
            .map_err(|source| PrefixParseError {
                input: s.to_owned(),
                source,
            })
    }
}

impl fmt::Display for IpPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IpPrefix::V4(net) => write!(f, "{net}"),
            IpPrefix::V6(net) => write!(f, "{net}"),
        }
    }
}

impl Serialize for IpPrefix {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for IpPrefix {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Borrow the string when possible to avoid an allocation on the hot path.
        let raw: &str = <&str>::deserialize(deserializer)?;
        IpPrefix::from_str(raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ipv4_prefix() {
        let p: IpPrefix = "192.0.2.0/24".parse().unwrap();
        assert!(p.is_ipv4());
        assert_eq!(p.prefix_len(), 24);
        assert_eq!(p.to_string(), "192.0.2.0/24");
    }

    #[test]
    fn parses_ipv6_prefix() {
        let p: IpPrefix = "2001:db8::/32".parse().unwrap();
        assert!(p.is_ipv6());
        assert_eq!(p.prefix_len(), 32);
    }

    #[test]
    fn rejects_garbage() {
        let err = "not-a-prefix".parse::<IpPrefix>().unwrap_err();
        assert!(err.to_string().contains("invalid IP prefix"));
    }

    #[test]
    fn serde_round_trip() {
        let p: IpPrefix = "203.0.113.0/24".parse().unwrap();
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "\"203.0.113.0/24\"");
        let back: IpPrefix = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
