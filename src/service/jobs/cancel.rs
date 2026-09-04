//! Cancel pending job runs.

use crate::{
    core::Registry,
    db::{DbConnection, query},
    service::{ServiceContext, ServiceError},
};

/// Cancel all pending jobs, optionally filtered by slug.
/// Returns the number of cancelled jobs.
///
/// # Errors
///
/// Returns a backend error if the DELETE fails.
pub fn cancel_pending_jobs(
    conn: &dyn DbConnection,
    slug: Option<&str>,
) -> Result<i64, ServiceError> {
    query::jobs::cancel_pending_jobs(conn, slug).map_err(ServiceError::Internal)
}

/// Cancel ONE pending run, authorized exactly like reading it: a queued
/// bulk run by its queuer (or an override caller), a user job's run by the
/// job's own `access` gate. A claimed/running job cannot be stopped, so
/// only `pending` rows are cancellable.
///
/// Returns `Ok(false)` when the run does not exist, is not visible to the
/// caller, or is no longer pending — the same "hide existence" shape
/// [`super::get_job_run`] uses.
///
/// # Errors
///
/// Returns a backend error if the lookup or DELETE fails.
pub fn cancel_job_run(
    ctx: &ServiceContext,
    registry: &Registry,
    id: &str,
) -> Result<bool, ServiceError> {
    // Reuse the read gate verbatim: whoever may see a run may cancel it.
    if super::get_job_run(ctx, registry, id)?.is_none() {
        return Ok(false);
    }

    let conn = ctx.resolve_conn()?;

    query::jobs::cancel_pending_job(conn.as_ref(), id).map_err(ServiceError::Internal)
}
