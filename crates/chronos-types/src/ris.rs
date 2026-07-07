//! RIPE RIS Live message models.
//!
//! Reference stream: `ws://ris-live.ripe.net/v1/stream/`.
//!
//! The public envelope looks like:
//! ```json
//! {
//!   "type": "ris_message",
//!   "data": {
//!     "timestamp": 1700000000.5,
//!     "peer": "10.0.0.1",
//!     "peer_asn": "12345",
//!     "type": "UPDATE",
//!     "path": [1234, 5678, [64500, 64501]],
//!     "origin": "igp",
//!     "announcements": [{ "next_hop": "10.0.0.1", "prefixes": ["192.0.2.0/24"] }],
//!     "withdrawals": ["198.51.100.0/24"]
//!   }
//! }
//! ```
//!
//! Design notes:
//! - Fields are optional with defaults so that unrelated control frames (for
//!   example `ris_error` or `pong`) do not cause a hard parse failure; callers
//!   inspect `RisEnvelope::kind` and `RisData::msg_type` to decide what to keep.
//! - `peer_asn` arrives as a JSON string; it is parsed into an `Asn`.
//! - `path` may contain AS_SET segments (nested arrays); those are flattened so
//!   that a single sequence of `Asn` values is always produced.

use crate::asn::Asn;
use crate::prefix::IpPrefix;
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt;

/// The BGP level message type carried inside `data.type`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RisMessageType {
    /// A BGP UPDATE (carries announcements and/or withdrawals).
    Update,
    /// A BGP OPEN message.
    Open,
    /// A BGP NOTIFICATION message.
    Notification,
    /// A BGP KEEPALIVE message.
    KeepAlive,
    /// A RIS peer state change notice.
    RisPeerState,
    /// Any other or missing type; the raw string is preserved when present.
    #[default]
    Other,
}

impl<'de> Deserialize<'de> for RisMessageType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: &str = <&str>::deserialize(deserializer)?;
        Ok(match raw {
            "UPDATE" => RisMessageType::Update,
            "OPEN" => RisMessageType::Open,
            "NOTIFICATION" => RisMessageType::Notification,
            "KEEPALIVE" => RisMessageType::KeepAlive,
            "RIS_PEER_STATE" => RisMessageType::RisPeerState,
            _ => RisMessageType::Other,
        })
    }
}

/// The outer RIS Live envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct RisEnvelope {
    /// The envelope type (for example `ris_message`, `ris_error`, `pong`).
    #[serde(rename = "type")]
    pub kind: String,
    /// The payload; defaulted so that non message envelopes still deserialize.
    #[serde(default)]
    pub data: RisData,
}

impl RisEnvelope {
    /// Return true when this envelope carries a routing message.
    #[inline]
    pub fn is_ris_message(&self) -> bool {
        self.kind == "ris_message"
    }
}

/// A single announcement block (a shared next hop for one or more prefixes).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RisAnnouncement {
    /// The BGP next hop for the listed prefixes (kept as a string; not all
    /// consumers need it parsed).
    #[serde(default)]
    pub next_hop: Option<String>,
    /// Prefixes announced with this next hop.
    #[serde(default)]
    pub prefixes: Vec<IpPrefix>,
}

/// The routing payload of a `ris_message` envelope.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RisData {
    /// Unix timestamp (seconds, fractional) reported by the collector.
    #[serde(default)]
    pub timestamp: f64,
    /// The peer IP address that sent the message.
    #[serde(default)]
    pub peer: Option<String>,
    /// The peer ASN; RIS Live encodes this as a string.
    #[serde(default, deserialize_with = "deserialize_optional_asn")]
    pub peer_asn: Option<Asn>,
    /// The BGP message type.
    #[serde(rename = "type", default)]
    pub msg_type: RisMessageType,
    /// The AS_PATH, flattened across any AS_SET segments.
    #[serde(default, deserialize_with = "deserialize_path")]
    pub path: Vec<Asn>,
    /// The BGP origin attribute (for example `igp`, `egp`, `incomplete`).
    #[serde(default)]
    pub origin: Option<String>,
    /// Announcement blocks.
    #[serde(default)]
    pub announcements: Vec<RisAnnouncement>,
    /// Withdrawn prefixes.
    #[serde(default)]
    pub withdrawals: Vec<IpPrefix>,
}

impl RisData {
    /// Return the origin ASN (the last element of the AS_PATH), if any.
    #[inline]
    pub fn origin_asn(&self) -> Option<Asn> {
        self.path.last().copied()
    }

    /// Iterate over every announced prefix across all announcement blocks.
    pub fn announced_prefixes(&self) -> impl Iterator<Item = IpPrefix> + '_ {
        self.announcements
            .iter()
            .flat_map(|block| block.prefixes.iter().copied())
    }
}

/// A convenience alias used by consumers that only handle routing messages.
pub type RisMessage = RisData;

/// Parse `peer_asn` from its JSON string (or numeric) representation.
fn deserialize_optional_asn<'de, D>(deserializer: D) -> Result<Option<Asn>, D::Error>
where
    D: Deserializer<'de>,
{
    struct AsnVisitor;

    impl<'de> Visitor<'de> for AsnVisitor {
        type Value = Option<Asn>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an ASN as a string or unsigned integer")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D>(self, d: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            d.deserialize_any(AsnValueVisitor).map(Some)
        }
    }

    deserializer.deserialize_option(AsnVisitor)
}

struct AsnValueVisitor;

impl<'de> Visitor<'de> for AsnValueVisitor {
    type Value = Asn;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an ASN as a string or unsigned integer")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        v.trim()
            .parse::<u32>()
            .map(Asn)
            .map_err(|_| de::Error::custom(format!("invalid ASN string: {v}")))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        u32::try_from(v)
            .map(Asn)
            .map_err(|_| de::Error::custom("ASN out of u32 range"))
    }
}

/// Deserialize an AS_PATH that may contain AS_SET segments (nested arrays).
///
/// The RIS Live path is a JSON array whose elements are either integers or, for
/// AS_SET segments, arrays of integers. Both forms are flattened into a single
/// sequence of `Asn` values.
fn deserialize_path<'de, D>(deserializer: D) -> Result<Vec<Asn>, D::Error>
where
    D: Deserializer<'de>,
{
    struct PathVisitor;

    impl<'de> Visitor<'de> for PathVisitor {
        type Value = Vec<Asn>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an AS_PATH array of integers and/or nested integer arrays")
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
            while let Some(element) = seq.next_element::<PathElement>()? {
                match element {
                    PathElement::Single(asn) => out.push(asn),
                    PathElement::Set(set) => out.extend(set),
                }
            }
            Ok(out)
        }
    }

    deserializer.deserialize_seq(PathVisitor)
}

/// One element of an AS_PATH: a single ASN or an AS_SET (array of ASNs).
#[derive(Deserialize)]
#[serde(untagged)]
enum PathElement {
    Single(#[serde(deserialize_with = "asn_from_number")] Asn),
    Set(Vec<Asn>),
}

fn asn_from_number<'de, D>(deserializer: D) -> Result<Asn, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u32::deserialize(deserializer)?;
    Ok(Asn(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "type": "ris_message",
        "data": {
            "timestamp": 1700000000.5,
            "peer": "10.0.0.1",
            "peer_asn": "64500",
            "type": "UPDATE",
            "path": [64500, 64501, [64502, 64503]],
            "origin": "igp",
            "announcements": [
                { "next_hop": "10.0.0.1", "prefixes": ["192.0.2.0/24", "2001:db8::/32"] }
            ],
            "withdrawals": ["198.51.100.0/24"]
        }
    }"#;

    #[test]
    fn parses_full_update() {
        let env: RisEnvelope = serde_json::from_str(SAMPLE).unwrap();
        assert!(env.is_ris_message());
        let d = &env.data;
        assert_eq!(d.msg_type, RisMessageType::Update);
        assert_eq!(d.peer_asn, Some(Asn(64500)));
        // AS_SET is flattened into the path.
        assert_eq!(d.path, vec![Asn(64500), Asn(64501), Asn(64502), Asn(64503)]);
        assert_eq!(d.origin_asn(), Some(Asn(64503)));
        assert_eq!(d.announced_prefixes().count(), 2);
        assert_eq!(d.withdrawals.len(), 1);
    }

    #[test]
    fn tolerates_control_envelope() {
        // A ris_error envelope has a different data shape; it must not hard fail.
        let raw = r#"{"type":"ris_error","data":{"message":"bad subscription"}}"#;
        let env: RisEnvelope = serde_json::from_str(raw).unwrap();
        assert!(!env.is_ris_message());
        assert_eq!(env.data.msg_type, RisMessageType::Other);
    }

    #[test]
    fn tolerates_missing_optional_fields() {
        let raw = r#"{"type":"ris_message","data":{"type":"UPDATE"}}"#;
        let env: RisEnvelope = serde_json::from_str(raw).unwrap();
        assert!(env.data.path.is_empty());
        assert!(env.data.announcements.is_empty());
        assert_eq!(env.data.peer_asn, None);
    }

    #[test]
    fn rejects_malformed_json() {
        let raw = r#"{"type":"ris_message","data":{"type":"UPDATE","#;
        assert!(serde_json::from_str::<RisEnvelope>(raw).is_err());
    }

    #[test]
    fn rejects_bad_prefix_in_announcement() {
        let raw = r#"{"type":"ris_message","data":{"type":"UPDATE",
            "announcements":[{"prefixes":["not-a-prefix"]}]}}"#;
        assert!(serde_json::from_str::<RisEnvelope>(raw).is_err());
    }

    #[test]
    fn accepts_numeric_peer_asn() {
        let raw = r#"{"type":"ris_message","data":{"type":"UPDATE","peer_asn":64500}}"#;
        let env: RisEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(env.data.peer_asn, Some(Asn(64500)));
    }
}
