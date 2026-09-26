//! Per-section validation methods on [`CrapConfig`](crate::config::CrapConfig).
//! The `validate()` orchestrator lives in `types.rs`; each helper here checks
//! one section and records every problem it finds in an
//! [`ErrorReport`](crate::config::ErrorReport), so one section's problems are
//! all reported together and test cases stay narrow.

mod cors;
mod limits;
mod redis;
mod server;
mod services;
