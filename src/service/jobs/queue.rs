//! Queue a job run with optional access control.

use serde_json::{Value, from_str, from_value};

use crate::{
    core::{
        DocumentFields,
        job::{JobDefinition, JobRun},
    },
    db::{AccessResult, DbConnection, query},
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
    /// Seconds to wait before the run becomes claimable. `0` = immediately.
    pub delay_secs: u64,
    /// Dedup key: when another pending/running run of this job carries the
    /// same key, that run is returned instead of queuing a duplicate.
    pub unique_key: Option<&'a str>,
}

/// Queue a new job run, enforcing access control if configured. The ONE
/// queue chokepoint for **caller-triggered** runs — gRPC `TriggerJob`, MCP
/// `trigger_job`, and `crap.jobs.queue` all pass through here, so the
/// access rules, the payload contract, and the delay/unique semantics
/// cannot drift between them.
///
/// System inserts stay separate by design, each with its own pinned
/// contract: the cron scheduler (definition-driven, no access hook),
/// `bulk_queue::queue_bulk` (access checked against the *collection* op at
/// queue time; `max_attempts` hard-pinned to 1 so a committed batch can
/// never be re-applied), and the email / image-convert queues.
///
/// If `job_def.access` is set, the job's Lua access function decides whether
/// `ctx.user` may trigger this job, with the queued payload exposed as
/// `ctx.data`. Returns `ServiceError::AccessDenied` when it denies.
///
/// # Errors
///
/// Returns `AccessDenied` when the access hook denies, `HookError` (an
/// invalid-argument on the wire) when `data` is not valid JSON — or not an
/// object while a data-gating access rule needs to inspect it — and a
/// backend error if the access check or INSERT fails.
pub fn queue_job(ctx: &ServiceContext, input: &QueueJobInput) -> Result<JobRun, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();

    // Fail at queue time, not at execution: a payload that is not valid
    // JSON used to be stored verbatim (gRPC sends a raw string) and only
    // blew up when the handler ran, long after the caller was gone.
    let payload = parse_payload(input.data);

    if input.job_def.access.is_some() {
        let fields = match &payload {
            Ok(value) => payload_fields(value.clone()),
            Err(_) => Ok(None),
        };

        // The rule runs before the payload is rejected — against no data when
        // it is unreadable — so a malformed payload is reported only to a
        // caller the rule lets through, and can't tell a denied caller that
        // the job exists.
        check_trigger_access(
            ctx,
            conn,
            input,
            fields.as_ref().ok().and_then(Option::as_ref),
        )?;

        fields?;
    }

    payload?;

    let inserted = query::jobs::insert_job_with(
        conn,
        &query::jobs::InsertJobOpts {
            slug: ctx.slug,
            data: input.data.unwrap_or("{}"),
            scheduled_by: input.scheduled_by,
            max_attempts: input.job_def.effective_max_attempts(input.queue_retries),
            queue: &input.job_def.queue,
            priority: input.priority,
            delay_secs: input.delay_secs,
            unique_key: input.unique_key,
        },
    )
    .map_err(ServiceError::Internal)?;

    Ok(inserted.into_inner())
}

/// The queued payload as JSON — `None` without data.
fn parse_payload(data: Option<&str>) -> Result<Option<Value>, ServiceError> {
    data.map(from_str::<Value>)
        .transpose()
        .map_err(|e| ServiceError::HookError(format!("job data must be valid JSON: {e}")))
}

/// The payload as the fields an access rule reads as `ctx.data`. A non-object
/// payload is an error: dropping it to nil would let a data-gating rule
/// evaluate against nothing while the job still queued with that payload.
fn payload_fields(value: Option<Value>) -> Result<Option<DocumentFields>, ServiceError> {
    value
        .map(from_value::<DocumentFields>)
        .transpose()
        .map_err(|_| {
            ServiceError::HookError(
                "job data must be a JSON object so the job's access rule can inspect it"
                    .to_string(),
            )
        })
}

/// Run the job's access rule for `ctx.user`, with the queued payload exposed as
/// `ctx.data` so it can gate on *what* is queued, not only *who* queues it.
fn check_trigger_access(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    input: &QueueJobInput,
    payload: Option<&DocumentFields>,
) -> Result<(), ServiceError> {
    let access_input = AccessCheckInput::builder("trigger", ctx.slug)
        .access(input.job_def.access.as_ref())
        .user(ctx.user)
        .data(payload)
        .build();

    // One rule, two evaluators — the same split every CRUD path uses. A
    // context carrying `write_hooks` evaluates through them
    // (`LuaWriteHooks` runs in the CALLER's VM, so `crap.jobs.queue`
    // inside a hook never re-enters the VM pool); otherwise the runner
    // is used directly, which is what gRPC and MCP do.
    let result = match ctx.write_hooks {
        Some(hooks) => hooks.check_access(&access_input),
        None => ctx.runner()?.check_access(&access_input, conn),
    }
    .map_err(ServiceError::Internal)?;

    match result {
        AccessResult::Allowed => Ok(()),
        AccessResult::Denied => Err(ServiceError::AccessDenied(
            "Trigger access denied".to_string(),
        )),
        AccessResult::Constrained(_) => Err(ServiceError::HookError(format!(
            "Access hook for job '{}' returned a filter table; job access is trigger-only — return true/false based on ctx.user fields instead.",
            ctx.slug
        ))),
    }
}
