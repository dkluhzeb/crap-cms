//! Queue a job run with optional access control.

use crate::{
    core::job::{JobDefinition, JobRun},
    db::{AccessResult, query},
    service::{ServiceContext, ServiceError},
};

/// Input for [`queue_job`].
pub struct QueueJobInput<'a> {
    pub job_def: &'a JobDefinition,
    pub data: Option<&'a str>,
    pub scheduled_by: &'a str,
}

/// Queue a new job run, enforcing access control if configured.
///
/// If `job_def.access` is set, the runner's Lua VM checks whether the given
/// `user` is allowed to trigger this job. Returns `ServiceError::AccessDenied`
/// when the check denies access.
pub fn queue_job(ctx: &ServiceContext, input: &QueueJobInput) -> Result<JobRun, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let runner = ctx.runner()?;

    if input.job_def.access.is_some() {
        let result = runner
            .check_access(input.job_def.access.as_deref(), ctx.user, None, None, conn)
            .map_err(ServiceError::Internal)?;

        if matches!(result, AccessResult::Denied) {
            return Err(ServiceError::AccessDenied(
                "Trigger access denied".to_string(),
            ));
        }
    }

    let job_run = query::jobs::insert_job(
        conn,
        ctx.slug,
        input.data.unwrap_or("{}"),
        input.scheduled_by,
        input.job_def.retries + 1,
        &input.job_def.queue,
    )
    .map_err(ServiceError::Internal)?;

    Ok(job_run)
}
