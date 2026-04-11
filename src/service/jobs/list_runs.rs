//! List job runs with optional filters.

use crate::{
    core::job::JobRun,
    db::{DbConnection, query},
    service::{PaginatedResult, ServiceError},
};

/// Input for [`list_job_runs`].
pub struct ListJobRunsInput<'a> {
    pub slug: Option<&'a str>,
    pub status: Option<&'a str>,
    pub limit: i64,
    pub offset: i64,
}

/// List job runs, optionally filtered by slug and/or status.
pub fn list_job_runs(
    conn: &dyn DbConnection,
    input: &ListJobRunsInput,
) -> Result<PaginatedResult<JobRun>, ServiceError> {
    let total = query::jobs::count_job_runs(conn, input.slug, input.status)
        .map_err(ServiceError::Internal)?;

    let runs =
        query::jobs::list_job_runs(conn, input.slug, input.status, input.limit, input.offset)
            .map_err(ServiceError::Internal)?;

    let page = if input.limit > 0 {
        input.offset / input.limit + 1
    } else {
        1
    };

    let pagination =
        query::PaginationResult::builder(&[], total, input.limit).page(page, input.offset);

    Ok(PaginatedResult {
        docs: runs,
        total,
        pagination,
    })
}
