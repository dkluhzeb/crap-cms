//! List job runs, gated by per-job read access.

use crate::{
    core::{JobRun, JobStatus, Registry},
    db::query::{self, PaginationResult},
    service::{PaginatedResult, ServiceContext, ServiceError, jobs::access::readable_slugs},
};

/// Input for [`list_job_runs`].
pub struct ListJobRunsInput<'a> {
    pub registry: &'a Registry,
    pub slug: Option<&'a str>,
    pub status: Option<&'a str>,
    pub limit: i64,
    pub offset: i64,
}

/// List job runs, restricted to the jobs whose runs `ctx.user` may read.
///
/// Reads are a permissive union, never an error: an unknown or denied
/// `slug` yields an empty page (hiding existence); `slug = None` returns runs
/// only for jobs the caller may read. See [`crate::service::jobs::access`].
///
/// # Errors
///
/// Returns `HookError` from a job access hook (a denial downgrades to an empty
/// page, never an error), or a backend error if the COUNT or SELECT fails.
pub fn list_job_runs(
    ctx: &ServiceContext,
    input: &ListJobRunsInput,
) -> Result<PaginatedResult<JobRun>, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();

    let allowed = readable_slugs(ctx, conn, input.registry, input.slug)?;

    let page = if input.limit > 0 {
        input.offset / input.limit + 1
    } else {
        1
    };

    let empty_page = || {
        let pagination = PaginationResult::builder(&[], 0, input.limit).page(page, input.offset);
        PaginatedResult {
            docs: Vec::new(),
            total: 0,
            pagination,
        }
    };

    // Resolve the requested status string to the typed DB filter at this one
    // boundary. An unknown status matches nothing (an empty page) — the same
    // result the raw-string SQL comparison produced before the query layer took
    // a typed `JobStatus`.
    let status = match input.status {
        None => None,
        Some(s) => match JobStatus::from_name(s) {
            Some(js) => Some(js),
            None => return Ok(empty_page()),
        },
    };

    if allowed.is_empty() {
        return Ok(empty_page());
    }

    let total =
        query::jobs::count_job_runs_in(conn, &allowed, status).map_err(ServiceError::Internal)?;

    let runs = query::jobs::list_job_runs_in(conn, &allowed, status, input.limit, input.offset)
        .map_err(ServiceError::Internal)?;

    let pagination = PaginationResult::builder(&[], total, input.limit).page(page, input.offset);

    Ok(PaginatedResult {
        docs: runs,
        total,
        pagination,
    })
}
