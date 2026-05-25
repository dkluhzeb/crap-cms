//! CRUD query functions for the `_crap_jobs` and `_crap_cron_fired` tables.

mod bulk;
mod claim;
mod cron;
mod lifecycle;
mod query;

#[cfg(test)]
mod test_helpers;

pub use bulk::{cancel_pending_jobs, purge_old_jobs};
pub use claim::claim_pending_jobs;
pub use cron::try_claim_cron_window;
pub use lifecycle::{
    InsertJobOpts, InsertedJob, complete_job, fail_job, insert_job, insert_job_with, mark_stale,
    update_heartbeat,
};
pub use query::{
    count_failed_since, count_job_runs, count_pending_older_than, count_running,
    count_running_per_queue, count_running_per_slug, find_stale_jobs, get_job_run,
    last_completed_run, list_job_runs,
};
