//! Get a single job run by ID.

use crate::{
    core::job::JobRun,
    db::{DbConnection, query},
    service::ServiceError,
};

/// Retrieve a single job run by its ID.
pub fn get_job_run(conn: &dyn DbConnection, id: &str) -> Result<Option<JobRun>, ServiceError> {
    query::jobs::get_job_run(conn, id).map_err(ServiceError::Internal)
}
