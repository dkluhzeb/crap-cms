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

    let Some(job_def) = registry.get_job(&run.slug) else {
        return Ok(None);
    };

    if !can_read_job_runs(ctx, conn, job_def, &run.slug)? {
        return Ok(None);
    }

    Ok(Some(run))
}
