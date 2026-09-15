//! Job execution, cron scheduling, stale recovery, cron normalization, and soft-delete purge.
//!
//! ## Submodule layout
//!
//! - `execute.rs` -- `execute_job` dispatch (Lua handler, system email, and
//!   the hand-off to the image-convert and bulk system jobs).
//! - `failure.rs` -- the single job-failure write path (retryable and
//!   permanent).
//! - `image_convert.rs` -- the `_system_image_convert` job: encode, URL
//!   write, completion, and the change report.
//! - `cron_schedule.rs` -- `check_cron_schedules`.
//! - `cron_expr.rs` -- 5-field cron normalization.
//! - `stale.rs` -- `recover_stale_jobs`.
//! - `retention.rs` -- soft-delete retention purge and its tick claim.

mod cron_expr;
mod cron_schedule;
mod execute;
mod failure;
mod image_convert;
mod retention;
mod stale;

#[cfg(all(test, feature = "sqlite"))]
mod test_support;

pub use cron_schedule::check_cron_schedules;
pub use execute::{ExecuteJobParams, execute_job};
pub use retention::purge_soft_deleted;
pub use stale::recover_stale_jobs;

pub(super) use failure::record_permanent_job_failure;
pub(super) use retention::claim_retention_purge_tick;
