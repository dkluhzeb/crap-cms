//! List job runs with optional filters.

use crate::{
    core::job::JobRun,
    db::{DbConnection, query},
    service::ServiceError,
};

/// List job runs, optionally filtered by slug and/or status.
pub fn list_job_runs(
    conn: &dyn DbConnection,
    slug: Option<&str>,
    status: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<JobRun>, ServiceError> {
    query::jobs::list_job_runs(conn, slug, status, limit, offset).map_err(ServiceError::Internal)
}
