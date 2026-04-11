//! Email lookup, password hash, session version, and user existence checks.

use anyhow::{Context as _, Result};

use crate::{
    core::{CollectionDefinition, Document, HashedPassword, auth::hash_password},
    db::{DbConnection, DbValue, document::row_to_document, query::get_column_names},
};

/// Find a document by email in an auth collection.
pub fn find_by_email(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    email: &str,
) -> Result<Option<Document>> {
    let column_names = get_column_names(def);
    let sql = format!(
        "SELECT {} FROM \"{}\" WHERE email = {}",
        column_names.join(", "),
        slug,
        conn.placeholder(1)
    );

    let Some(row) = conn.query_one(&sql, &[DbValue::Text(email.to_string())])? else {
        return Ok(None);
    };

    Ok(Some(row_to_document(conn, &row)?))
}

/// Get the password hash for a document by ID. Returns None if no hash set.
pub fn get_password_hash(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
) -> Result<Option<HashedPassword>> {
    let sql = format!(
        "SELECT _password_hash FROM \"{slug}\" WHERE id = {}",
        conn.placeholder(1)
    );

    let Some(row) = conn.query_one(&sql, &[DbValue::Text(id.to_string())])? else {
        return Ok(None);
    };

    Ok(row
        .get_opt_string("_password_hash")?
        .map(HashedPassword::new))
}

/// Update the password hash for a document by ID.
/// Hashes the plaintext password before storing.
pub fn update_password(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
    password: &str,
) -> Result<()> {
    let hash = hash_password(password)?;
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
    let sql = format!(
        "UPDATE \"{slug}\" SET _password_hash = {p1}, \
         _session_version = COALESCE(_session_version, 0) + 1 WHERE id = {p2}"
    );
    conn.execute(
        &sql,
        &[
            DbValue::Text(hash.as_ref().to_string()),
            DbValue::Text(id.to_string()),
        ],
    )
    .with_context(|| format!("Failed to update password for {id} in {slug}"))?;
    Ok(())
}

/// Check whether a user has a password set (non-NULL `_password_hash`).
pub fn has_password(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<bool> {
    let sql = format!(
        "SELECT (_password_hash IS NOT NULL) AS has_pw FROM \"{slug}\" WHERE id = {}",
        conn.placeholder(1)
    );

    conn.query_one(&sql, &[DbValue::Text(id.to_string())])?
        .map(|row| row.get_bool("has_pw"))
        .transpose()
        .map(|opt| opt.unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::config::CrapConfig;
    use crate::core::collection::*;
    use crate::core::field::*;
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
                id TEXT PRIMARY KEY, email TEXT UNIQUE, name TEXT,
                _password_hash TEXT, _session_version INTEGER DEFAULT 0,
                created_at TEXT, updated_at TEXT
            );
            INSERT INTO users (id, email, name, created_at, updated_at)
            VALUES ('user1', 'test@example.com', 'Test User', '2024-01-01', '2024-01-01');",
        )
        .unwrap();
        (dir, conn)
    }

    fn auth_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("users");
        def.fields = vec![
            FieldDefinition::builder("email", FieldType::Email)
                .required(true)
                .unique(true)
                .build(),
            FieldDefinition::builder("name", FieldType::Text).build(),
        ];
        def
    }

    #[test]
    fn find_by_email_found() {
        let (_dir, conn) = setup();
        let result = find_by_email(&conn, "users", &auth_def(), "test@example.com").unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "user1");
    }

    #[test]
    fn find_by_email_not_found() {
        let (_dir, conn) = setup();
        let result = find_by_email(&conn, "users", &auth_def(), "nobody@example.com").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_password_hash_none() {
        let (_dir, conn) = setup();
        assert!(
            get_password_hash(&conn, "users", "user1")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn update_password_then_get() {
        let (_dir, conn) = setup();
        update_password(&conn, "users", "user1", "secret123").unwrap();
        let hash = get_password_hash(&conn, "users", "user1").unwrap().unwrap();
        let hash_str: &str = hash.as_ref();
        assert!(hash_str.starts_with("$argon2"));
    }

    #[test]
    fn has_password_false_when_no_hash() {
        let (_dir, conn) = setup();
        assert!(!has_password(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn has_password_true_after_set() {
        let (_dir, conn) = setup();
        update_password(&conn, "users", "user1", "secret123").unwrap();
        assert!(has_password(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn has_password_nonexistent_user() {
        let (_dir, conn) = setup();
        assert!(!has_password(&conn, "users", "nonexistent").unwrap());
    }

    #[test]
    fn get_password_hash_nonexistent_user() {
        let (_dir, conn) = setup();
        assert!(
            get_password_hash(&conn, "users", "nonexistent")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn update_password_increments_session_version() {
        use crate::db::query::auth::get_session_version;

        let (_dir, conn) = setup();
        assert_eq!(get_session_version(&conn, "users", "user1").unwrap(), 0);
        update_password(&conn, "users", "user1", "pass1").unwrap();
        assert_eq!(get_session_version(&conn, "users", "user1").unwrap(), 1);
        update_password(&conn, "users", "user1", "pass2").unwrap();
        assert_eq!(get_session_version(&conn, "users", "user1").unwrap(), 2);
    }

    #[test]
    fn update_password_increments_from_null() {
        use crate::db::query::auth::get_session_version;

        let (_dir, conn) = setup();
        conn.execute(
            "UPDATE users SET _session_version = NULL WHERE id = 'user1'",
            &[],
        )
        .unwrap();
        update_password(&conn, "users", "user1", "newpass").unwrap();
        assert_eq!(get_session_version(&conn, "users", "user1").unwrap(), 1);
    }
}
