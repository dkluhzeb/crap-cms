//! Soft-delete retention purge and its multi-node tick claim.
//!
//! - `period.rs` -- the `soft_delete_retention` duration grammar.
//! - `run.rs` -- `RetentionPurge`: a purge run's batches, resume point and
//!   set-aside documents.
//! - `purge.rs` -- `purge_soft_deleted`: one bounded batch of the purge.
//! - `claim.rs` -- `claim_retention_purge_tick`: the single-winner claim of a
//!   purge window.

mod claim;
mod period;
mod purge;
mod run;

pub use purge::purge_soft_deleted;
pub use run::{PurgeBatch, RetentionPurge};

pub(in crate::scheduler) use claim::claim_retention_purge_tick;
