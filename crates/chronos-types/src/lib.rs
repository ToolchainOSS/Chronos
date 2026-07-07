//! Core primitive types shared across the Chronos crates.
//!
//! Design goals (see project blueprint Phase 1):
//! - Autonomous System Numbers (ASNs) are represented as `u32` via a thin newtype
//!   (`Asn`) so they stay stack allocated and cheap to copy.
//! - IP prefixes use a compact `IpPrefix` enum wrapping `ipnetwork` network types;
//!   both variants are fixed size (address bytes plus a prefix length) so ingestion
//!   never heap allocates per prefix.
//!
//! Style note: this codebase avoids em dashes in comments and docs; it uses
//! colons, semicolons, and parentheses instead.

mod asn;
mod delta;
mod prefix;
mod ris;

pub use asn::Asn;
pub use delta::Delta;
pub use prefix::{IpPrefix, PrefixParseError};
pub use ris::{RisAnnouncement, RisData, RisEnvelope, RisMessage, RisMessageType};
