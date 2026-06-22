//! Queue a job run with optional access control.

use crate::{
    core::{
        DocumentFields,
        job::{JobDefinition, JobRun},
    },
    db::{AccessResult, query},
    hooks::AccessCheckInput,
    service::{ServiceContext, ServiceError},
};

/// Input for [`queue_job`].
pub struct QueueJobInput<'a> {
    pub job_def: &'a JobDefinition,
    pub data: Option<&'a str>,
    pub scheduled_by: &'a str,
    /// Static scheduling priority; higher = sooner. `0` = standard FIFO.
    pub priority: i32,
    /// Queue-level retries default (`[jobs.queues.<queue>] retries`),
    /// used as the fallback when `job_def.retries` is `None`. Pass
    /// `None` if the caller has no `JobsConfig` access — the
    /// definition's explicit retries still applies; the fallback is
    /// `0` (one attempt).
    pub queue_retries: Option<u32>,
}

/// Queue a new job run, enforcing access control if configured.
///
/// If `job_def.access` is set, the runner's Lua VM checks whether the given
/// `user` is allowed to trigger this job. Returns `ServiceError::AccessDenied`
/// when the check denies access.
///
/// # Errors
///
/// Returns `AccessDenied` when the access hook denies, or a backend error if
/// the access check or INSERT fails.
pub fn queue_job(ctx: &ServiceContext, input: &QueueJobInput) -> Result<JobRun, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let runner = ctx.runner()?;

    if input.job_def.access.is_some() {
        // Expose the queued payload to the access fn as `ctx.data`, so it can
        // gate on *what* is being queued, not only *who* is queuing.
        let payload = input
            .data
            .and_then(|s| serde_json::from_str::<DocumentFields>(s).ok());

        let result = runner
            .check_access(
                &AccessCheckInput {
                    document: None,
                    access: input.job_def.access.as_ref(),
                    user: ctx.user,
                    id: None,
                    data: payload.as_ref(),
                    locale: None,
                    operation: "trigger",
                    collection: ctx.slug,
                    ui_locale: None,
                },
                conn,
            )
            .map_err(ServiceError::Internal)?;

        if matches!(result, AccessResult::Denied) {
            return Err(ServiceError::AccessDenied(
                "Trigger access denied".to_string(),
            ));
        }

        if matches!(result, AccessResult::Constrained(_)) {
            return Err(ServiceError::HookError(format!(
                "Access hook for job '{}' returned a filter table; job access is trigger-only — return true/false based on ctx.user fields instead.",
                ctx.slug
            )));
        }
    }

    let job_run = query::jobs::insert_job(
        conn,
        ctx.slug,
        input.data.unwrap_or("{}"),
        input.scheduled_by,
        input.job_def.effective_max_attempts(input.queue_retries),
        &input.job_def.queue,
        input.priority,
    )
    .map_err(ServiceError::Internal)?;

    Ok(job_run)
}
