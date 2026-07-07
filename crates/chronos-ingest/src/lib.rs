//! RIPE RIS Live ingestion client.
//!
//! Responsibilities (blueprint Phase 1 and Phase 2, Task 2.1):
//! - Connect asynchronously to `ws://ris-live.ripe.net/v1/ws/`.
//! - Subscribe to UPDATE messages (withdrawals arrive inside UPDATE frames on RIS
//!   Live; there is no separate WITHDRAW subscription type).
//! - Parse each frame into strongly typed structures.
//! - Push parsed messages into a bounded channel so a slow consumer applies
//!   backpressure without ever blocking the network socket (a full channel drops
//!   the frame and increments a counter rather than stalling the reader).
//! - Reconnect automatically with exponential backoff plus jitter.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

mod client;
mod config;
mod parse;

pub use client::{IngestStats, run_ingest};
pub use config::IngestConfig;
pub use parse::{ParseError, parse_message, subscribe_message};
