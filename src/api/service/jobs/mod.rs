//! Job RPC handlers: ListJobs, TriggerJob, GetJobRun, ListJobRuns.

mod get_run;
mod list;
mod list_runs;
mod trigger;

use crate::{api::content, core::job::JobRun};

/// Convert a JobRun to gRPC response.
#[cfg(not(tarpaulin_include))]
pub(super) fn job_run_to_proto(run: &JobRun) -> content::GetJobRunResponse {
    content::GetJobRunResponse {
        id: run.id.clone(),
        slug: run.slug.clone(),
        status: run.status.as_str().to_string(),
        data_json: run.data.clone(),
        result_json: run.result.clone(),
        error: run.error.clone(),
        attempt: run.attempt,
        max_attempts: run.max_attempts,
        scheduled_by: run.scheduled_by.clone(),
        created_at: run.created_at.clone(),
        started_at: run.started_at.clone(),
        completed_at: run.completed_at.clone(),
    }
}
