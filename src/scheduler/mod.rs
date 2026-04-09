//! Background job scheduler: polls for pending jobs, evaluates cron schedules,
//! executes Lua handlers, and manages heartbeats and stale recovery.

mod loop_runner;
mod runner;
mod types;

pub use loop_runner::start;
pub use runner::{check_cron_schedules, execute_job, purge_soft_deleted, recover_stale_jobs};
pub use types::{SchedulerParams, SchedulerParamsBuilder};
