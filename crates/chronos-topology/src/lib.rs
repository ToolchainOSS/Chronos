//! In-memory topology structures for Project Chronos (blueprint Phase 2).
//!
//! Two structures are provided:
//! - `PrefixTable`: a binary radix (Patricia style) trie mapping IP prefixes to
//!   the origin ASN that announces them, with enough history to detect origin
//!   changes; wrapped for concurrent reads and atomic writes.
//! - `AsGraph`: an adjacency list (`DashMap<u32, HashSet<u32>>`) describing active
//!   peering relationships derived from AS_PATH attributes.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

mod graph;
mod trie;

pub use graph::{AsGraph, EdgeChange};
pub use trie::{OriginObservation, PrefixTable};
