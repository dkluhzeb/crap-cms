//! User settings (preferences, locale, column visibility).
//!
//! Operates on the `_crap_user_settings` table — not auth-related.

use anyhow::{Context as _, Result};

use crate::db::{DbConnection, DbValue};

/// Get user settings JSON blob. Returns None if no settings saved.
pub fn get_user_settings(conn: &dyn DbConnection, user_id: &str) -> Result<Option<String>> {
    let sql = format!(
        "SELECT settings FROM _crap_user_settings WHERE user_id = {}",
        conn.placeholder(1)
    );

    conn.query_one(&sql, &[DbValue::Text(user_id.to_string())])?
        .map(|row| row.get_string("settings"))
        .transpose()
}

/// Set user settings JSON blob (UPSERT).
pub fn set_user_settings(
    conn: &dyn DbConnection,
    user_id: &str,
    settings_json: &str,
) -> Result<()> {
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
    let sql = format!(
        "INSERT INTO _crap_user_settings (user_id, settings) VALUES ({p1}, {p2})
         ON CONFLICT(user_id) DO UPDATE SET settings = excluded.settings"
    );

    conn.execute(
        &sql,
        &[
            DbValue::Text(user_id.to_string()),
            DbValue::Text(settings_json.to_string()),
        ],
    )
    .with_context(|| format!("Failed to set settings for user {user_id}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::config::CrapConfig;
    use crate::db::{BoxedConnection, DbConnection, pool};

    fn setup_conn() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let db_pool = pool::create_pool(dir.path(), &config).unwrap();
        let conn = db_pool.get().unwrap();
        (dir, conn)
    }

    fn setup_table(conn: &dyn DbConnection) {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _crap_user_settings (
                user_id TEXT PRIMARY KEY,
                settings TEXT NOT NULL DEFAULT '{}'
            );",
        )
        .unwrap();
    }

    #[test]
    fn get_none_when_no_row() {
        let (_dir, conn) = setup_conn();
        setup_table(&conn);
        let result = get_user_settings(&conn, "user1").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn set_then_get() {
        let (_dir, conn) = setup_conn();
        setup_table(&conn);
        let settings = r#"{"posts":{"columns":["title","status"]}}"#;
        set_user_settings(&conn, "user1", settings).unwrap();
        let result = get_user_settings(&conn, "user1").unwrap();
        assert_eq!(result.as_deref(), Some(settings));
    }

    #[test]
    fn set_overwrites() {
        let (_dir, conn) = setup_conn();
        setup_table(&conn);
        set_user_settings(&conn, "user1", r#"{"a":1}"#).unwrap();
        set_user_settings(&conn, "user1", r#"{"b":2}"#).unwrap();
        let result = get_user_settings(&conn, "user1").unwrap();
        assert_eq!(result.as_deref(), Some(r#"{"b":2}"#));
    }

    #[test]
    fn get_nonexistent_user() {
        let (_dir, conn) = setup_conn();
        setup_table(&conn);
        let result = get_user_settings(&conn, "nonexistent").unwrap();
        assert!(result.is_none());
    }
}
