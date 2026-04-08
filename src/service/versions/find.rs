//! Find a specific version by ID.

use crate::{
    core::document::VersionSnapshot,
    db::{DbConnection, query},
    service::ServiceError,
};

/// Look up a single version snapshot by its ID.
pub fn find_version_by_id(
    conn: &dyn DbConnection,
    slug: &str,
    version_id: &str,
) -> Result<Option<VersionSnapshot>, ServiceError> {
    Ok(query::find_version_by_id(conn, slug, version_id)?)
}
