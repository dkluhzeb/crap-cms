//! Job RPC handlers: `ListJobs`, `TriggerJob`, `GetJobRun`, `ListJobRuns`.

mod get_run;
mod list;
mod list_runs;
mod trigger;

use crate::{
    api::{content, handlers::enum_mapping},
    core::job::JobRun,
};

/// Convert a `JobRun` to its gRPC `JobRunInfo` representation.
#[cfg(not(tarpaulin_include))]
pub(super) fn job_run_to_proto(run: &JobRun) -> content::JobRunInfo {
    content::JobRunInfo {
        id: run.id.clone(),
        slug: run.slug.clone(),
        status: enum_mapping::job_run_status(run.status).into(),
        data_json: run.data.clone(),
        result_json: run.result.clone(),
        error: run.error.clone(),
        attempt: run.attempt,
        max_attempts: run.max_attempts,
        scheduled_by: enum_mapping::job_scheduled_by(run.scheduled_by.as_deref()).into(),
        created_at: run.created_at.clone(),
        started_at: run.started_at.clone(),
        completed_at: run.completed_at.clone(),
        priority: run.priority,
        unique_key: run.unique_key.clone(),
    }
}
