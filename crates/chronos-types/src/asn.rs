//! Autonomous System Number newtype.

use serde::{Deserialize, Serialize};
use std::fmt;

/// An Autonomous System Number.
///
/// Represented as a `u32` (supports 32 bit ASNs per RFC 6793); the newtype keeps
/// the value stack allocated and prevents accidental mixing with other integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Asn(pub u32);

impl Asn {
    /// Construct an ASN from its raw numeric value.
    #[inline]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Return the underlying numeric value.
    #[inline]
    pub const fn value(self) -> u32 {
        self.0
    }
}

impl From<u32> for Asn {
    #[inline]
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Asn> for u32 {
    #[inline]
    fn from(asn: Asn) -> Self {
        asn.0
    }
}

impl fmt::Display for Asn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AS{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_u32() {
        let asn = Asn::new(64512);
        assert_eq!(asn.value(), 64512);
        assert_eq!(u32::from(asn), 64512);
        assert_eq!(Asn::from(13335u32), Asn(13335));
    }

    #[test]
    fn displays_with_as_prefix() {
        assert_eq!(Asn(3356).to_string(), "AS3356");
    }

    #[test]
    fn serializes_transparently() {
        let json = serde_json::to_string(&Asn(15169)).unwrap();
        assert_eq!(json, "15169");
        let parsed: Asn = serde_json::from_str("15169").unwrap();
        assert_eq!(parsed, Asn(15169));
    }
}
