//! Streaming anomaly detection heuristics (blueprint Phase 3).
//!
//! Three deterministic, rule based heuristics are provided:
//! - Origin validation (Task 3.1): a prefix announced by a new origin ASN is a
//!   possible prefix hijack.
//! - Path leak detection (Task 3.2): an AS_PATH that violates valley free routing
//!   policy is a possible route leak. Relationships come from a pluggable
//!   provider (CAIDA AS relationship data when mounted; a degree based heuristic
//!   otherwise).
//! - Temporal surge monitoring (Task 3.3): a ring buffer sliding window measures
//!   per prefix update velocity and flags route churn when it crosses a high pass
//!   Median Absolute Deviation (MAD) threshold.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

mod anomaly;
mod origin;
mod pathleak;
mod relationships;
mod surge;

pub use anomaly::{Anomaly, Severity};
pub use origin::check_origin;
pub use pathleak::PathLeakDetector;
pub use relationships::{
    parse_caida_as_rel, CaidaRelationships, DegreeHeuristic, Relationship, RelationshipProvider,
};
pub use surge::{SurgeConfig, SurgeMonitor};
