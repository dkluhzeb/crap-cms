//! Auth-related query functions: email lookup, password, reset tokens, verification.

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

    match conn.query_one(&sql, &[DbValue::Text(email.to_string())])? {
        Some(row) => Ok(Some(row_to_document(conn, &row)?)),
        None => Ok(None),
    }
}

/// Get the password hash for a document by ID. Returns None if no hash set.
pub fn get_password_hash(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
) -> Result<Option<HashedPassword>> {
    let sql = format!(
        "SELECT _password_hash FROM \"{}\" WHERE id = {}",
        slug,
        conn.placeholder(1)
    );

    match conn.query_one(&sql, &[DbValue::Text(id.to_string())])? {
        Some(row) => Ok(row
            .get_opt_string("_password_hash")?
            .map(HashedPassword::new)),
        None => Ok(None),
    }
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
        "UPDATE \"{}\" SET _password_hash = {}, _session_version = COALESCE(_session_version, 0) + 1 WHERE id = {}",
        slug, p1, p2
    );
    conn.execute(
        &sql,
        &[
            DbValue::Text(hash.as_ref().to_string()),
            DbValue::Text(id.to_string()),
        ],
    )
    .with_context(|| format!("Failed to update password for {} in {}", id, slug))?;
    Ok(())
}

/// Check whether a user has a password set (non-NULL `_password_hash`).
pub fn has_password(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<bool> {
    let sql = format!(
        "SELECT (_password_hash IS NOT NULL) AS has_pw FROM \"{}\" WHERE id = {}",
        slug,
        conn.placeholder(1)
    );
    match conn.query_one(&sql, &[DbValue::Text(id.to_string())])? {
        Some(row) => row.get_bool("has_pw"),
        None => Ok(false),
    }
}

// ── Reset token functions ─────────────────────────────────────────────────

/// Store a password reset token and expiry for a user.
pub fn set_reset_token(
    conn: &dyn DbConnection,
    slug: &str,
    user_id: &str,
    token: &str,
    exp: i64,
) -> Result<()> {
    let (p1, p2, p3) = (
        conn.placeholder(1),
        conn.placeholder(2),
        conn.placeholder(3),
    );
    let sql = format!(
        "UPDATE \"{}\" SET _reset_token = {}, _reset_token_exp = {} WHERE id = {}",
        slug, p1, p2, p3
    );
    conn.execute(
        &sql,
        &[
            DbValue::Text(token.to_string()),
            DbValue::Integer(exp),
            DbValue::Text(user_id.to_string()),
        ],
    )
    .with_context(|| format!("Failed to set reset token for {} in {}", user_id, slug))?;
    Ok(())
}

/// Find a user by their reset token. Returns the document and token expiry.
pub fn find_by_reset_token(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    token: &str,
) -> Result<Option<(Document, i64)>> {
    let column_names = get_column_names(def);
    let cols = column_names.join(", ");
    let sql = format!(
        "SELECT {}, _reset_token_exp FROM \"{}\" WHERE _reset_token = {}",
        cols,
        slug,
        conn.placeholder(1)
    );

    match conn.query_one(&sql, &[DbValue::Text(token.to_string())])? {
        Some(row) => {
            let doc = row_to_document(conn, &row)?;
            let exp = row
                .get_i64("_reset_token_exp")
                .context("Failed to read _reset_token_exp")?;
            Ok(Some((doc, exp)))
        }
        None => Ok(None),
    }
}

/// Clear the reset token for a user (after successful reset or expiry).
pub fn clear_reset_token(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<()> {
    let sql = format!(
        "UPDATE \"{}\" SET _reset_token = NULL, _reset_token_exp = NULL WHERE id = {}",
        slug,
        conn.placeholder(1)
    );
    conn.execute(&sql, &[DbValue::Text(user_id.to_string())])
        .with_context(|| format!("Failed to clear reset token for {} in {}", user_id, slug))?;
    Ok(())
}

// ── Email verification functions ──────────────────────────────────────────

/// Store a verification token and expiry for a user.
pub fn set_verification_token(
    conn: &dyn DbConnection,
    slug: &str,
    user_id: &str,
    token: &str,
    exp: i64,
) -> Result<()> {
    let (p1, p2, p3) = (
        conn.placeholder(1),
        conn.placeholder(2),
        conn.placeholder(3),
    );
    let sql = format!(
        "UPDATE \"{}\" SET _verification_token = {}, _verification_token_exp = {} WHERE id = {}",
        slug, p1, p2, p3
    );
    conn.execute(
        &sql,
        &[
            DbValue::Text(token.to_string()),
            DbValue::Integer(exp),
            DbValue::Text(user_id.to_string()),
        ],
    )
    .with_context(|| {
        format!(
            "Failed to set verification token for {} in {}",
            user_id, slug
        )
    })?;
    Ok(())
}

/// Find a user by their verification token. Returns the document and token expiry.
pub fn find_by_verification_token(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    token: &str,
) -> Result<Option<(Document, i64)>> {
    let column_names = get_column_names(def);
    let cols = column_names.join(", ");
    let sql = format!(
        "SELECT {}, _verification_token_exp FROM \"{}\" WHERE _verification_token = {}",
        cols,
        slug,
        conn.placeholder(1)
    );

    match conn.query_one(&sql, &[DbValue::Text(token.to_string())])? {
        Some(row) => {
            let doc = row_to_document(conn, &row)?;
            let exp = row
                .get_i64("_verification_token_exp")
                .context("Failed to read _verification_token_exp")?;
            Ok(Some((doc, exp)))
        }
        None => Ok(None),
    }
}

/// Clear the verification token for a user (after expiry). Does NOT change `_verified` status.
pub fn clear_verification_token(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<()> {
    let sql = format!(
        "UPDATE \"{}\" SET _verification_token = NULL, _verification_token_exp = NULL WHERE id = {}",
        slug,
        conn.placeholder(1)
    );
    conn.execute(&sql, &[DbValue::Text(user_id.to_string())])
        .with_context(|| {
            format!(
                "Failed to clear verification token for {} in {}",
                user_id, slug
            )
        })?;
    Ok(())
}

/// Mark a user as verified (set _verified = 1, clear token and expiry).
pub fn mark_verified(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<()> {
    let sql = format!(
        "UPDATE \"{}\" SET _verified = 1, _verification_token = NULL, _verification_token_exp = NULL WHERE id = {}",
        slug,
        conn.placeholder(1)
    );
    conn.execute(&sql, &[DbValue::Text(user_id.to_string())])
        .with_context(|| format!("Failed to mark user {} as verified in {}", user_id, slug))?;
    Ok(())
}

/// Mark a user as unverified (set _verified = 0). Does NOT touch token fields.
pub fn mark_unverified(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<()> {
    let sql = format!(
        "UPDATE \"{}\" SET _verified = 0 WHERE id = {}",
        slug,
        conn.placeholder(1)
    );
    conn.execute(&sql, &[DbValue::Text(user_id.to_string())])
        .with_context(|| format!("Failed to mark user {} as unverified in {}", user_id, slug))?;
    Ok(())
}

// ── User settings functions ────────────────────────────────────────────────

/// Get user settings JSON blob from `_crap_user_settings` table.
/// Returns None if no settings saved.
pub fn get_user_settings(conn: &dyn DbConnection, user_id: &str) -> Result<Option<String>> {
    let sql = format!(
        "SELECT settings FROM _crap_user_settings WHERE user_id = {}",
        conn.placeholder(1)
    );
    match conn.query_one(&sql, &[DbValue::Text(user_id.to_string())])? {
        Some(row) => Ok(Some(row.get_string("settings")?)),
        None => Ok(None),
    }
}

/// Set user settings JSON blob (UPSERT into `_crap_user_settings`).
pub fn set_user_settings(
    conn: &dyn DbConnection,
    user_id: &str,
    settings_json: &str,
) -> Result<()> {
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
    // ON CONFLICT ... DO UPDATE (UPSERT) is valid for both SQLite (>= 3.24) and PostgreSQL (>= 9.5).
    let sql = format!(
        "INSERT INTO _crap_user_settings (user_id, settings) VALUES ({}, {})
         ON CONFLICT(user_id) DO UPDATE SET settings = excluded.settings",
        p1, p2
    );
    conn.execute(
        &sql,
        &[
            DbValue::Text(user_id.to_string()),
            DbValue::Text(settings_json.to_string()),
        ],
    )
    .with_context(|| format!("Failed to set settings for user {}", user_id))?;
    Ok(())
}

/// Check whether a user exists in the given collection.
pub fn user_exists(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<bool> {
    let sql = format!(
        "SELECT 1 FROM \"{}\" WHERE id = {}",
        slug,
        conn.placeholder(1)
    );

    Ok(conn
        .query_one(&sql, &[DbValue::Text(id.to_string())])?
        .is_some())
}

// ── Lock/unlock functions ─────────────────────────────────────────────────

/// Lock a user account (prevent login).
pub fn lock_user(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<()> {
    let sql = format!(
        "UPDATE \"{}\" SET _locked = 1 WHERE id = {}",
        slug,
        conn.placeholder(1)
    );
    conn.execute(&sql, &[DbValue::Text(id.to_string())])
        .with_context(|| format!("Failed to lock user {} in {}", id, slug))?;
    Ok(())
}

/// Unlock a user account (allow login).
pub fn unlock_user(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<()> {
    let sql = format!(
        "UPDATE \"{}\" SET _locked = 0 WHERE id = {}",
        slug,
        conn.placeholder(1)
    );
    conn.execute(&sql, &[DbValue::Text(id.to_string())])
        .with_context(|| format!("Failed to unlock user {} in {}", id, slug))?;
    Ok(())
}

/// Get the session version for a user. Returns 0 if no version set (NULL or missing).
pub fn get_session_version(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<u64> {
    let sql = format!(
        "SELECT COALESCE(_session_version, 0) AS sv FROM \"{}\" WHERE id = {}",
        slug,
        conn.placeholder(1)
    );

    match conn.query_one(&sql, &[DbValue::Text(id.to_string())])? {
        Some(row) => Ok(row.get_i64("sv")? as u64),
        None => Ok(0),
    }
}

/// Check if a user account is locked.
pub fn is_locked(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<bool> {
    let sql = format!(
        "SELECT _locked FROM \"{}\" WHERE id = {}",
        slug,
        conn.placeholder(1)
    );
    match conn.query_one(&sql, &[DbValue::Text(id.to_string())])? {
        Some(row) => Ok(row.get_bool("_locked")?),
        None => Ok(false),
    }
}

/// Check if a user is verified.
pub fn is_verified(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<bool> {
    let sql = format!(
        "SELECT _verified FROM \"{}\" WHERE id = {}",
        slug,
        conn.placeholder(1)
    );
    match conn.query_one(&sql, &[DbValue::Text(user_id.to_string())])? {
        Some(row) => Ok(row.get_bool("_verified")?),
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::{BoxedConnection, DbConnection, pool};
    use tempfile::TempDir;

    fn setup_conn() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let db_pool = pool::create_pool(dir.path(), &config).unwrap();
        let conn = db_pool.get().unwrap();
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

    fn setup_auth_db(conn: &dyn DbConnection) {
        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                email TEXT UNIQUE,
                name TEXT,
                _password_hash TEXT,
                _reset_token TEXT,
                _reset_token_exp INTEGER,
                _locked INTEGER DEFAULT 0,
                _settings TEXT,
                _session_version INTEGER DEFAULT 0,
                _verification_token TEXT,
                _verification_token_exp INTEGER,
                _verified INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO users (id, email, name, created_at, updated_at)
            VALUES ('user1', 'test@example.com', 'Test User', '2024-01-01', '2024-01-01');",
        )
        .unwrap();
    }

    // ── find_by_email tests ─────────────────────────────────────────────────

    #[test]
    fn find_by_email_found() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let def = auth_def();
        let result = find_by_email(&conn, "users", &def, "test@example.com").unwrap();
        assert!(result.is_some(), "Should find existing user by email");
        let doc = result.unwrap();
        assert_eq!(doc.id, "user1");
    }

    #[test]
    fn find_by_email_not_found() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let def = auth_def();
        let result = find_by_email(&conn, "users", &def, "nobody@example.com").unwrap();
        assert!(
            result.is_none(),
            "Should return None for non-existent email"
        );
    }

    // ── get_password_hash + update_password tests ───────────────────────────

    #[test]
    fn get_password_hash_none() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let result = get_password_hash(&conn, "users", "user1").unwrap();
        assert!(
            result.is_none(),
            "Should return None when no password hash is set"
        );
    }

    #[test]
    fn update_password_then_get() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        update_password(&conn, "users", "user1", "secret123").unwrap();
        let hash = get_password_hash(&conn, "users", "user1").unwrap();
        assert!(hash.is_some(), "Should return Some after setting password");
        let hash_val = hash.unwrap();
        let hash_str: &str = hash_val.as_ref();
        assert!(!hash_str.is_empty(), "Hash should be non-empty");
        // Argon2 hashes start with $argon2
        assert!(
            hash_str.starts_with("$argon2"),
            "Hash should be an Argon2 hash"
        );
    }

    // ── set_reset_token + find_by_reset_token tests ─────────────────────────

    #[test]
    fn set_and_find_reset_token() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let def = auth_def();
        let exp = 9999999999i64;
        set_reset_token(&conn, "users", "user1", "reset-abc", exp).unwrap();
        let result = find_by_reset_token(&conn, "users", &def, "reset-abc").unwrap();
        assert!(result.is_some(), "Should find user by reset token");
        let (doc, returned_exp) = result.unwrap();
        assert_eq!(doc.id, "user1");
        assert_eq!(returned_exp, exp);
    }

    #[test]
    fn find_by_reset_token_wrong() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let def = auth_def();
        set_reset_token(&conn, "users", "user1", "reset-abc", 9999999999).unwrap();
        let result = find_by_reset_token(&conn, "users", &def, "wrong-token").unwrap();
        assert!(result.is_none(), "Should return None for wrong reset token");
    }

    #[test]
    fn find_by_reset_token_expired() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let def = auth_def();
        // Set token with a past expiry (the DB function doesn't check expiry)
        let past_exp = 1000i64;
        set_reset_token(&conn, "users", "user1", "reset-expired", past_exp).unwrap();
        let result = find_by_reset_token(&conn, "users", &def, "reset-expired").unwrap();
        assert!(
            result.is_some(),
            "DB function should return expired tokens (caller checks expiry)"
        );
        let (doc, returned_exp) = result.unwrap();
        assert_eq!(doc.id, "user1");
        assert_eq!(returned_exp, past_exp);
    }

    // ── clear_reset_token tests ─────────────────────────────────────────────

    #[test]
    fn clear_reset_token_works() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let def = auth_def();
        set_reset_token(&conn, "users", "user1", "reset-xyz", 9999999999).unwrap();
        clear_reset_token(&conn, "users", "user1").unwrap();
        let result = find_by_reset_token(&conn, "users", &def, "reset-xyz").unwrap();
        assert!(
            result.is_none(),
            "Should return None after clearing reset token"
        );
    }

    // ── set_verification_token + find_by_verification_token tests ───────────

    #[test]
    fn set_and_find_verification_token() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let def = auth_def();
        let exp = 9999999999i64;
        set_verification_token(&conn, "users", "user1", "verify-abc", exp).unwrap();
        let result = find_by_verification_token(&conn, "users", &def, "verify-abc").unwrap();
        assert!(result.is_some(), "Should find user by verification token");
        let (doc, returned_exp) = result.unwrap();
        assert_eq!(doc.id, "user1");
        assert_eq!(returned_exp, exp);
    }

    #[test]
    fn find_by_verification_token_wrong() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let def = auth_def();
        set_verification_token(&conn, "users", "user1", "verify-abc", 9999999999).unwrap();
        let result = find_by_verification_token(&conn, "users", &def, "wrong-token").unwrap();
        assert!(
            result.is_none(),
            "Should return None for wrong verification token"
        );
    }

    #[test]
    fn find_by_verification_token_expired() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let def = auth_def();
        let past_exp = 1000i64;
        set_verification_token(&conn, "users", "user1", "verify-expired", past_exp).unwrap();
        let result = find_by_verification_token(&conn, "users", &def, "verify-expired").unwrap();
        assert!(
            result.is_some(),
            "DB function should return expired tokens (caller checks expiry)"
        );
        let (doc, returned_exp) = result.unwrap();
        assert_eq!(doc.id, "user1");
        assert_eq!(returned_exp, past_exp);
    }

    // ── mark_verified + is_verified tests ───────────────────────────────────

    #[test]
    fn is_verified_default_false() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let result = is_verified(&conn, "users", "user1").unwrap();
        assert!(!result, "Newly created user should not be verified");
    }

    #[test]
    fn mark_verified_then_check() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        mark_verified(&conn, "users", "user1").unwrap();
        let result = is_verified(&conn, "users", "user1").unwrap();
        assert!(result, "User should be verified after mark_verified");
    }

    // ── lock/unlock tests ─────────────────────────────────────────────────

    // ── user_exists tests ─────────────────────────────────────────────────

    #[test]
    fn user_exists_true_for_existing() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        assert!(user_exists(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn user_exists_false_for_missing() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        assert!(!user_exists(&conn, "users", "nonexistent").unwrap());
    }

    /// Regression test: is_locked returns Ok(false) for non-existent users,
    /// which means a deleted user would appear as "not locked". The session
    /// refresh handler must check user_exists separately.
    #[test]
    fn is_locked_false_for_deleted_user_proves_need_for_existence_check() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        assert!(!is_locked(&conn, "users", "deleted-user-id").unwrap());
        assert!(!user_exists(&conn, "users", "deleted-user-id").unwrap());
    }

    // ── lock/unlock tests ─────────────────────────────────────────────────

    #[test]
    fn is_locked_default_false() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let result = is_locked(&conn, "users", "user1").unwrap();
        assert!(!result, "Newly created user should not be locked");
    }

    #[test]
    fn lock_then_check() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        lock_user(&conn, "users", "user1").unwrap();
        let result = is_locked(&conn, "users", "user1").unwrap();
        assert!(result, "User should be locked after lock_user");
    }

    #[test]
    fn lock_then_unlock() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        lock_user(&conn, "users", "user1").unwrap();
        assert!(is_locked(&conn, "users", "user1").unwrap());
        unlock_user(&conn, "users", "user1").unwrap();
        assert!(
            !is_locked(&conn, "users", "user1").unwrap(),
            "User should be unlocked after unlock_user"
        );
    }

    #[test]
    fn is_locked_nonexistent_user() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let result = is_locked(&conn, "users", "nonexistent").unwrap();
        assert!(!result, "Non-existent user should return false");
    }

    #[test]
    fn is_verified_nonexistent_user() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let result = is_verified(&conn, "users", "nonexistent").unwrap();
        assert!(!result, "Non-existent user should return false");
    }

    #[test]
    fn mark_unverified_then_check() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        // Start verified
        mark_verified(&conn, "users", "user1").unwrap();
        assert!(is_verified(&conn, "users", "user1").unwrap());

        // Unverify
        mark_unverified(&conn, "users", "user1").unwrap();
        assert!(
            !is_verified(&conn, "users", "user1").unwrap(),
            "User should not be verified after mark_unverified"
        );
    }

    #[test]
    fn mark_verified_then_unverify_preserves_token() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let def = auth_def();
        // Set a verification token, then verify, then unverify
        set_verification_token(&conn, "users", "user1", "verify-tok", 9999999999).unwrap();
        mark_verified(&conn, "users", "user1").unwrap();

        // mark_verified clears token
        let found = find_by_verification_token(&conn, "users", &def, "verify-tok").unwrap();
        assert!(found.is_none(), "mark_verified should clear token");

        // Unverify — should not re-create a token
        mark_unverified(&conn, "users", "user1").unwrap();
        assert!(!is_verified(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn clear_verification_token_removes_token_but_not_verified_status() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let def = auth_def();

        // Set a verification token
        set_verification_token(&conn, "users", "user1", "verify-clear", 9999999999).unwrap();
        let found = find_by_verification_token(&conn, "users", &def, "verify-clear").unwrap();
        assert!(found.is_some(), "Token should exist before clearing");

        // Clear the token
        clear_verification_token(&conn, "users", "user1").unwrap();
        let found = find_by_verification_token(&conn, "users", &def, "verify-clear").unwrap();
        assert!(
            found.is_none(),
            "Token should be gone after clear_verification_token"
        );

        // User should still NOT be verified (clear_verification_token doesn't change _verified)
        assert!(
            !is_verified(&conn, "users", "user1").unwrap(),
            "clear_verification_token should not mark user as verified"
        );
    }

    #[test]
    fn mark_verified_clears_token() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let def = auth_def();
        set_verification_token(&conn, "users", "user1", "verify-abc", 9999999999).unwrap();

        // Verify token is set
        let found = find_by_verification_token(&conn, "users", &def, "verify-abc").unwrap();
        assert!(found.is_some());

        // Mark as verified -- should clear the token and expiry
        mark_verified(&conn, "users", "user1").unwrap();

        let found_after = find_by_verification_token(&conn, "users", &def, "verify-abc").unwrap();
        assert!(
            found_after.is_none(),
            "verification token should be cleared after mark_verified"
        );

        assert!(is_verified(&conn, "users", "user1").unwrap());
    }

    // ── has_password tests ─────────────────────────────────────────────────

    #[test]
    fn has_password_false_when_no_hash() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        assert!(!has_password(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn has_password_true_after_set() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        update_password(&conn, "users", "user1", "secret123").unwrap();
        assert!(has_password(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn has_password_nonexistent_user() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        assert!(!has_password(&conn, "users", "nonexistent").unwrap());
    }

    #[test]
    fn get_password_hash_nonexistent_user() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let result = get_password_hash(&conn, "users", "nonexistent").unwrap();
        assert!(result.is_none(), "Non-existent user should return None");
    }

    #[test]
    fn is_locked_null_value_treated_as_false() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        // user1 has _locked DEFAULT 0 which is an integer, but let's test NULL directly
        conn.execute("UPDATE users SET _locked = NULL WHERE id = 'user1'", &[])
            .unwrap();
        let result = is_locked(&conn, "users", "user1").unwrap();
        assert!(!result, "NULL _locked should be treated as false");
    }

    #[test]
    fn is_verified_null_value_treated_as_false() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        conn.execute("UPDATE users SET _verified = NULL WHERE id = 'user1'", &[])
            .unwrap();
        let result = is_verified(&conn, "users", "user1").unwrap();
        assert!(!result, "NULL _verified should be treated as false");
    }

    // ── user settings tests (_crap_user_settings table) ──────────────────────

    fn setup_user_settings_table(conn: &dyn DbConnection) {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _crap_user_settings (
                user_id TEXT PRIMARY KEY,
                settings TEXT NOT NULL DEFAULT '{}'
            );",
        )
        .unwrap();
    }

    #[test]
    fn get_user_settings_none_when_no_row() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        setup_user_settings_table(&conn);
        let result = get_user_settings(&conn, "user1").unwrap();
        assert!(
            result.is_none(),
            "Should return None when no settings saved"
        );
    }

    #[test]
    fn set_then_get_user_settings() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        setup_user_settings_table(&conn);
        let settings = r#"{"posts":{"columns":["title","status"]}}"#;
        set_user_settings(&conn, "user1", settings).unwrap();
        let result = get_user_settings(&conn, "user1").unwrap();
        assert_eq!(result.as_deref(), Some(settings));
    }

    #[test]
    fn set_user_settings_overwrites() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        setup_user_settings_table(&conn);
        set_user_settings(&conn, "user1", r#"{"a":1}"#).unwrap();
        set_user_settings(&conn, "user1", r#"{"b":2}"#).unwrap();
        let result = get_user_settings(&conn, "user1").unwrap();
        assert_eq!(result.as_deref(), Some(r#"{"b":2}"#));
    }

    #[test]
    fn get_user_settings_nonexistent_user() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        setup_user_settings_table(&conn);
        let result = get_user_settings(&conn, "nonexistent").unwrap();
        assert!(result.is_none(), "Non-existent user should return None");
    }

    // ── session version tests ─────────────────────────────────────────────────

    #[test]
    fn get_session_version_default_zero() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let version = get_session_version(&conn, "users", "user1").unwrap();
        assert_eq!(version, 0, "Default session version should be 0");
    }

    #[test]
    fn get_session_version_nonexistent_user() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);
        let version = get_session_version(&conn, "users", "nonexistent").unwrap();
        assert_eq!(version, 0, "Non-existent user should return 0");
    }

    #[test]
    fn update_password_increments_session_version() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);

        assert_eq!(get_session_version(&conn, "users", "user1").unwrap(), 0);

        update_password(&conn, "users", "user1", "pass1").unwrap();
        assert_eq!(get_session_version(&conn, "users", "user1").unwrap(), 1);

        update_password(&conn, "users", "user1", "pass2").unwrap();
        assert_eq!(get_session_version(&conn, "users", "user1").unwrap(), 2);
    }

    #[test]
    fn update_password_increments_from_null_session_version() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);

        // Set _session_version to NULL to simulate pre-migration rows
        conn.execute(
            "UPDATE users SET _session_version = NULL WHERE id = 'user1'",
            &[],
        )
        .unwrap();

        update_password(&conn, "users", "user1", "newpass").unwrap();
        assert_eq!(
            get_session_version(&conn, "users", "user1").unwrap(),
            1,
            "COALESCE should treat NULL as 0 then increment to 1"
        );
    }

    #[test]
    fn get_session_version_null_returns_zero() {
        let (_dir, conn) = setup_conn();
        setup_auth_db(&conn);

        conn.execute(
            "UPDATE users SET _session_version = NULL WHERE id = 'user1'",
            &[],
        )
        .unwrap();

        let version = get_session_version(&conn, "users", "user1").unwrap();
        assert_eq!(version, 0, "NULL _session_version should return 0");
    }
}
