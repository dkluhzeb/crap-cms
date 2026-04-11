//! Account status: lock/unlock, verified checks.

use anyhow::{Context as _, Result};

use crate::db::{DbConnection, DbValue};

/// Lock a user account (prevent login).
pub fn lock_user(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<()> {
    set_bool_column(conn, slug, id, "_locked", true)
}

/// Unlock a user account (allow login).
pub fn unlock_user(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<()> {
    set_bool_column(conn, slug, id, "_locked", false)
}

/// Check if a user account is locked.
pub fn is_locked(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<bool> {
    get_bool_column(conn, slug, id, "_locked")
}

/// Check if a user is verified.
pub fn is_verified(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<bool> {
    get_bool_column(conn, slug, user_id, "_verified")
}

/// Get the session version for a user. Returns 0 if no version set (NULL or missing).
pub fn get_session_version(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<u64> {
    let sql = format!(
        "SELECT COALESCE(_session_version, 0) AS sv FROM \"{slug}\" WHERE id = {}",
        conn.placeholder(1)
    );

    Ok(conn
        .query_one(&sql, &[DbValue::Text(id.to_string())])?
        .map(|row| row.get_i64("sv"))
        .transpose()?
        .unwrap_or(0) as u64)
}

/// Check whether a user exists in the given collection.
pub fn user_exists(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<bool> {
    let sql = format!(
        "SELECT 1 FROM \"{slug}\" WHERE id = {}",
        conn.placeholder(1)
    );

    Ok(conn
        .query_one(&sql, &[DbValue::Text(id.to_string())])?
        .is_some())
}

/// Read a boolean column for a user, returning false if the user doesn't exist.
fn get_bool_column(conn: &dyn DbConnection, slug: &str, id: &str, col: &str) -> Result<bool> {
    let sql = format!(
        "SELECT {col} FROM \"{slug}\" WHERE id = {}",
        conn.placeholder(1)
    );

    conn.query_one(&sql, &[DbValue::Text(id.to_string())])?
        .map(|row| row.get_bool(col))
        .transpose()
        .map(|opt| opt.unwrap_or(false))
}

/// Set a boolean column for a user.
fn set_bool_column(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
    col: &str,
    value: bool,
) -> Result<()> {
    let val = if value { 1 } else { 0 };
    let sql = format!(
        "UPDATE \"{slug}\" SET {col} = {val} WHERE id = {}",
        conn.placeholder(1)
    );
    conn.execute(&sql, &[DbValue::Text(id.to_string())])
        .with_context(|| format!("Failed to set {col} for {id} in {slug}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::config::CrapConfig;
    use crate::db::{BoxedConnection, DbConnection, pool};

    fn setup() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let conn = pool::create_pool(dir.path(), &config)
            .unwrap()
            .get()
            .unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                _locked INTEGER DEFAULT 0,
                _verified INTEGER DEFAULT 0,
                _session_version INTEGER DEFAULT 0
            );
            INSERT INTO users (id) VALUES ('user1');",
        )
        .unwrap();
        (dir, conn)
    }

    #[test]
    fn is_locked_default_false() {
        let (_dir, conn) = setup();
        assert!(!is_locked(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn lock_then_check() {
        let (_dir, conn) = setup();
        lock_user(&conn, "users", "user1").unwrap();
        assert!(is_locked(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn lock_then_unlock() {
        let (_dir, conn) = setup();
        lock_user(&conn, "users", "user1").unwrap();
        assert!(is_locked(&conn, "users", "user1").unwrap());
        unlock_user(&conn, "users", "user1").unwrap();
        assert!(!is_locked(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn is_locked_nonexistent() {
        let (_dir, conn) = setup();
        assert!(!is_locked(&conn, "users", "nonexistent").unwrap());
    }

    #[test]
    fn is_verified_default_false() {
        let (_dir, conn) = setup();
        assert!(!is_verified(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn is_verified_nonexistent() {
        let (_dir, conn) = setup();
        assert!(!is_verified(&conn, "users", "nonexistent").unwrap());
    }

    #[test]
    fn is_locked_null_treated_as_false() {
        let (_dir, conn) = setup();
        conn.execute("UPDATE users SET _locked = NULL WHERE id = 'user1'", &[])
            .unwrap();
        assert!(!is_locked(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn is_verified_null_treated_as_false() {
        let (_dir, conn) = setup();
        conn.execute("UPDATE users SET _verified = NULL WHERE id = 'user1'", &[])
            .unwrap();
        assert!(!is_verified(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn user_exists_true() {
        let (_dir, conn) = setup();
        assert!(user_exists(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn user_exists_false() {
        let (_dir, conn) = setup();
        assert!(!user_exists(&conn, "users", "nonexistent").unwrap());
    }

    #[test]
    fn get_session_version_default_zero() {
        let (_dir, conn) = setup();
        assert_eq!(get_session_version(&conn, "users", "user1").unwrap(), 0);
    }

    #[test]
    fn get_session_version_nonexistent() {
        let (_dir, conn) = setup();
        assert_eq!(
            get_session_version(&conn, "users", "nonexistent").unwrap(),
            0
        );
    }

    #[test]
    fn get_session_version_null_returns_zero() {
        let (_dir, conn) = setup();
        conn.execute(
            "UPDATE users SET _session_version = NULL WHERE id = 'user1'",
            &[],
        )
        .unwrap();
        assert_eq!(get_session_version(&conn, "users", "user1").unwrap(), 0);
    }

    #[test]
    fn is_locked_false_for_deleted_user_proves_need_for_existence_check() {
        let (_dir, conn) = setup();
        assert!(!is_locked(&conn, "users", "deleted-user-id").unwrap());
        assert!(!user_exists(&conn, "users", "deleted-user-id").unwrap());
    }
}
