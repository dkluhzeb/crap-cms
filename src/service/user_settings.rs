//! User settings service — per-user preferences (column visibility, locale, etc.).

use crate::db::{DbConnection, query};

use super::ServiceError;

/// Get a user's settings JSON string, or None if not set.
pub fn get_user_settings(
    conn: &dyn DbConnection,
    user_id: &str,
) -> Result<Option<String>, ServiceError> {
    Ok(query::get_user_settings(conn, user_id)?)
}

/// Save a user's settings JSON string (upsert).
pub fn set_user_settings(
    conn: &dyn DbConnection,
    user_id: &str,
    settings_json: &str,
) -> Result<(), ServiceError> {
    query::set_user_settings(conn, user_id, settings_json)?;
    Ok(())
}
