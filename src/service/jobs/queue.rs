//! Queue a job run with optional access control.

use crate::{
    core::{Document, job::JobDefinition, job::JobRun},
    db::{AccessResult, DbConnection, query},
    hooks::HookRunner,
    service::ServiceError,
};

/// Queue a new job run, enforcing access control if configured.
///
/// If `job_def.access` is set, the runner's Lua VM checks whether the given
/// `user` is allowed to trigger this job. Returns `ServiceError::AccessDenied`
/// when the check denies access.
pub fn queue_job(
    conn: &dyn DbConnection,
    runner: &HookRunner,
    slug: &str,
    job_def: &JobDefinition,
    data: Option<&str>,
    scheduled_by: &str,
    user: Option<&Document>,
) -> Result<JobRun, ServiceError> {
    if job_def.access.is_some() {
        let result = runner
            .check_access(job_def.access.as_deref(), user, None, None, conn)
            .map_err(ServiceError::Internal)?;

        if matches!(result, AccessResult::Denied) {
            return Err(ServiceError::AccessDenied(
                "Trigger access denied".to_string(),
            ));
        }
    }

    let job_run = query::jobs::insert_job(
        conn,
        slug,
        data.unwrap_or("{}"),
        scheduled_by,
        job_def.retries + 1,
        &job_def.queue,
    )
    .map_err(ServiceError::Internal)?;

    Ok(job_run)
}
