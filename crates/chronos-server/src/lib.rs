//! Project Chronos server library.
//!
//! The `chronos-server` binary is a thin wrapper around these modules. Exposing
//! them as a library lets the acceptance suite drive the real Axum router and
//! validate the Delta wire contract end to end, without duplicating the wiring.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

pub mod config;
pub mod hub;
pub mod metrics;
pub mod pipeline;
pub mod state;
