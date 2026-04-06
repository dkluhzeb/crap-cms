//! Reset and verification token lifecycle (set, find, clear, mark verified).

use anyhow::{Context as _, Result};

use crate::{
    core::{CollectionDefinition, Document},
    db::{DbConnection, DbValue, document::row_to_document, query::get_column_names},
};

// ── Shared token helpers ─────────────────────────────────────────────────

/// Store a token and expiry in the given columns for a user.
fn set_token(
    conn: &dyn DbConnection,
    slug: &str,
    user_id: &str,
    token_col: &str,
    exp_col: &str,
    token: &str,
    exp: i64,
) -> Result<()> {
    let (p1, p2, p3) = (
        conn.placeholder(1),
        conn.placeholder(2),
        conn.placeholder(3),
    );
    let sql = format!("UPDATE \"{slug}\" SET {token_col} = {p1}, {exp_col} = {p2} WHERE id = {p3}");
    conn.execute(
        &sql,
        &[
            DbValue::Text(token.to_string()),
            DbValue::Integer(exp),
            DbValue::Text(user_id.to_string()),
        ],
    )
    .with_context(|| format!("Failed to set {token_col} for {user_id} in {slug}"))?;
    Ok(())
}

/// Find a user by a token column. Returns the document and token expiry.
fn find_by_token(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    token_col: &str,
    exp_col: &str,
    token: &str,
) -> Result<Option<(Document, i64)>> {
    let cols = get_column_names(def).join(", ");
    let sql = format!(
        "SELECT {cols}, {exp_col} FROM \"{slug}\" WHERE {token_col} = {}",
        conn.placeholder(1)
    );

    let Some(row) = conn.query_one(&sql, &[DbValue::Text(token.to_string())])? else {
        return Ok(None);
    };

    let doc = row_to_document(conn, &row)?;
    let exp = row
        .get_i64(exp_col)
        .with_context(|| format!("Failed to read {exp_col}"))?;
    Ok(Some((doc, exp)))
}

/// Clear a token and its expiry column for a user.
fn clear_token(
    conn: &dyn DbConnection,
    slug: &str,
    user_id: &str,
    token_col: &str,
    exp_col: &str,
) -> Result<()> {
    let sql = format!(
        "UPDATE \"{slug}\" SET {token_col} = NULL, {exp_col} = NULL WHERE id = {}",
        conn.placeholder(1)
    );
    conn.execute(&sql, &[DbValue::Text(user_id.to_string())])
        .with_context(|| format!("Failed to clear {token_col} for {user_id} in {slug}"))?;
    Ok(())
}

// ── Reset token functions ────────────────────────────────────────────────

/// Store a password reset token and expiry for a user.
pub fn set_reset_token(
    conn: &dyn DbConnection,
    slug: &str,
    user_id: &str,
    token: &str,
    exp: i64,
) -> Result<()> {
    set_token(
        conn,
        slug,
        user_id,
        "_reset_token",
        "_reset_token_exp",
        token,
        exp,
    )
}

/// Find a user by their reset token. Returns the document and token expiry.
pub fn find_by_reset_token(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    token: &str,
) -> Result<Option<(Document, i64)>> {
    find_by_token(conn, slug, def, "_reset_token", "_reset_token_exp", token)
}

/// Clear the reset token for a user (after successful reset or expiry).
pub fn clear_reset_token(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<()> {
    clear_token(conn, slug, user_id, "_reset_token", "_reset_token_exp")
}

// ── Verification token functions ─────────────────────────────────────────

/// Store a verification token and expiry for a user.
pub fn set_verification_token(
    conn: &dyn DbConnection,
    slug: &str,
    user_id: &str,
    token: &str,
    exp: i64,
) -> Result<()> {
    set_token(
        conn,
        slug,
        user_id,
        "_verification_token",
        "_verification_token_exp",
        token,
        exp,
    )
}

/// Find a user by their verification token. Returns the document and token expiry.
pub fn find_by_verification_token(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    token: &str,
) -> Result<Option<(Document, i64)>> {
    find_by_token(
        conn,
        slug,
        def,
        "_verification_token",
        "_verification_token_exp",
        token,
    )
}

/// Clear the verification token for a user (after expiry). Does NOT change `_verified` status.
pub fn clear_verification_token(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<()> {
    clear_token(
        conn,
        slug,
        user_id,
        "_verification_token",
        "_verification_token_exp",
    )
}

/// Mark a user as verified (set _verified = 1, clear token and expiry).
pub fn mark_verified(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<()> {
    let sql = format!(
        "UPDATE \"{slug}\" SET _verified = 1, _verification_token = NULL, \
         _verification_token_exp = NULL WHERE id = {}",
        conn.placeholder(1)
    );
    conn.execute(&sql, &[DbValue::Text(user_id.to_string())])
        .with_context(|| format!("Failed to mark user {user_id} as verified in {slug}"))?;
    Ok(())
}

/// Mark a user as unverified (set _verified = 0). Does NOT touch token fields.
pub fn mark_unverified(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<()> {
    let sql = format!(
        "UPDATE \"{slug}\" SET _verified = 0 WHERE id = {}",
        conn.placeholder(1)
    );
    conn.execute(&sql, &[DbValue::Text(user_id.to_string())])
        .with_context(|| format!("Failed to mark user {user_id} as unverified in {slug}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::config::CrapConfig;
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::query::auth::{find_by_verification_token, is_verified, set_verification_token};
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
                _reset_token TEXT, _reset_token_exp INTEGER,
                _verification_token TEXT, _verification_token_exp INTEGER,
                _verified INTEGER DEFAULT 0,
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
    fn set_and_find_reset_token() {
        let (_dir, conn) = setup();
        let def = auth_def();
        set_reset_token(&conn, "users", "user1", "reset-abc", 9999999999).unwrap();
        let (doc, exp) = find_by_reset_token(&conn, "users", &def, "reset-abc")
            .unwrap()
            .unwrap();
        assert_eq!(doc.id, "user1");
        assert_eq!(exp, 9999999999);
    }

    #[test]
    fn find_by_reset_token_wrong() {
        let (_dir, conn) = setup();
        set_reset_token(&conn, "users", "user1", "reset-abc", 9999999999).unwrap();
        assert!(
            find_by_reset_token(&conn, "users", &auth_def(), "wrong-token")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn find_by_reset_token_expired() {
        let (_dir, conn) = setup();
        set_reset_token(&conn, "users", "user1", "reset-expired", 1000).unwrap();
        let result = find_by_reset_token(&conn, "users", &auth_def(), "reset-expired").unwrap();
        assert!(
            result.is_some(),
            "DB returns expired tokens (caller checks)"
        );
        assert_eq!(result.unwrap().1, 1000);
    }

    #[test]
    fn clear_reset_token_works() {
        let (_dir, conn) = setup();
        set_reset_token(&conn, "users", "user1", "reset-xyz", 9999999999).unwrap();
        clear_reset_token(&conn, "users", "user1").unwrap();
        assert!(
            find_by_reset_token(&conn, "users", &auth_def(), "reset-xyz")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn set_and_find_verification_token() {
        let (_dir, conn) = setup();
        set_verification_token(&conn, "users", "user1", "verify-abc", 9999999999).unwrap();
        let (doc, exp) = find_by_verification_token(&conn, "users", &auth_def(), "verify-abc")
            .unwrap()
            .unwrap();
        assert_eq!(doc.id, "user1");
        assert_eq!(exp, 9999999999);
    }

    #[test]
    fn find_by_verification_token_wrong() {
        let (_dir, conn) = setup();
        set_verification_token(&conn, "users", "user1", "verify-abc", 9999999999).unwrap();
        assert!(
            find_by_verification_token(&conn, "users", &auth_def(), "wrong-token")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn find_by_verification_token_expired() {
        let (_dir, conn) = setup();
        set_verification_token(&conn, "users", "user1", "verify-expired", 1000).unwrap();
        let result =
            find_by_verification_token(&conn, "users", &auth_def(), "verify-expired").unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().1, 1000);
    }

    #[test]
    fn mark_verified_then_check() {
        let (_dir, conn) = setup();
        mark_verified(&conn, "users", "user1").unwrap();
        assert!(is_verified(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn mark_unverified_then_check() {
        let (_dir, conn) = setup();
        mark_verified(&conn, "users", "user1").unwrap();
        mark_unverified(&conn, "users", "user1").unwrap();
        assert!(!is_verified(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn mark_verified_clears_token() {
        let (_dir, conn) = setup();
        let def = auth_def();
        set_verification_token(&conn, "users", "user1", "verify-abc", 9999999999).unwrap();
        mark_verified(&conn, "users", "user1").unwrap();
        assert!(
            find_by_verification_token(&conn, "users", &def, "verify-abc")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn clear_verification_token_does_not_change_verified_status() {
        let (_dir, conn) = setup();
        set_verification_token(&conn, "users", "user1", "verify-clear", 9999999999).unwrap();
        clear_verification_token(&conn, "users", "user1").unwrap();
        assert!(!is_verified(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn mark_verified_then_unverify_preserves_cleared_token() {
        let (_dir, conn) = setup();
        let def = auth_def();
        set_verification_token(&conn, "users", "user1", "verify-tok", 9999999999).unwrap();
        mark_verified(&conn, "users", "user1").unwrap();
        assert!(
            find_by_verification_token(&conn, "users", &def, "verify-tok")
                .unwrap()
                .is_none()
        );
        mark_unverified(&conn, "users", "user1").unwrap();
        assert!(!is_verified(&conn, "users", "user1").unwrap());
    }
}
