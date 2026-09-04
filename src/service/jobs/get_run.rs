//! Get a single job run by ID, gated by per-job read access.

use crate::{
    core::{JobRun, Registry},
    db::query,
    service::{ServiceContext, ServiceError, jobs::access::can_read_job_runs},
};

/// Retrieve a single job run by its ID, if `ctx.user` may read its job's runs.
///
/// Returns `Ok(None)` — hiding existence — when the run is missing, its job
/// definition is no longer registered (orphan run, fail closed), or the
/// caller is denied read access.
///
/// # Errors
///
/// Returns `HookError` propagated from a job access hook, or a backend error
/// if the SELECT fails.
pub fn get_job_run(
    ctx: &ServiceContext,
    registry: &Registry,
    id: &str,
) -> Result<Option<JobRun>, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();

    let Some(run) = query::jobs::get_job_run(conn, id).map_err(ServiceError::Internal)? else {
        return Ok(None);
    };

    // Queued-bulk system runs have no registered `JobDefinition`; they are
    // readable ONLY by the actor that queued them (or an override caller) —
    // the run's `data` carries the full request payload. Unparseable data
    // fails closed.
    if run.slug == crate::core::job::SYSTEM_BULK_JOB {
        let readable =
            serde_json::from_str::<super::bulk_queue::BulkJobData>(&run.data).is_ok_and(|d| {
                super::bulk_queue::can_read_bulk_run(&d.queued_by, ctx.user, ctx.override_access)
            });

        return Ok(readable.then_some(run));
    }

    let Some(job_def) = registry.get_job(&run.slug) else {
        return Ok(None);
    };

    if !can_read_job_runs(ctx, conn, job_def, &run.slug)? {
        return Ok(None);
    }

    Ok(Some(run))
}
