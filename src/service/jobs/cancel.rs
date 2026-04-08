//! Cancel pending job runs.

use crate::{
    db::{DbConnection, query},
    service::ServiceError,
};

/// Cancel all pending jobs, optionally filtered by slug.
/// Returns the number of cancelled jobs.
pub fn cancel_pending_jobs(
    conn: &dyn DbConnection,
    slug: Option<&str>,
) -> Result<i64, ServiceError> {
    query::jobs::cancel_pending_jobs(conn, slug).map_err(ServiceError::Internal)
}
