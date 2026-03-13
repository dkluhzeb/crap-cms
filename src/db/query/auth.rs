//! Auth-related query functions: email lookup, password, reset tokens, verification.

use anyhow::{Context as _, Result};

use super::get_column_names;
use crate::{
    core::{CollectionDefinition, Document, auth::hash_password},
    db::document::row_to_document,
};

/// Find a document by email in an auth collection.
pub fn find_by_email(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    email: &str,
) -> Result<Option<Document>> {
    let column_names = get_column_names(def);

    let sql = format!(
        "SELECT {} FROM {} WHERE email = ?1",
        column_names.join(", "),
        slug
    );

    let result = conn.query_row(&sql, [email], |row| row_to_document(row, &column_names));

    match result {
        Ok(doc) => Ok(Some(doc)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context(format!("Failed to find user by email in {}", slug)),
    }
}

/// Get the password hash for a document by ID. Returns None if no hash set.
pub fn get_password_hash(
    conn: &rusqlite::Connection,
    slug: &str,
    id: &str,
) -> Result<Option<String>> {
    let sql = format!("SELECT _password_hash FROM {} WHERE id = ?1", slug);

    let result = conn.query_row(&sql, [id], |row| row.get::<_, Option<String>>(0));

    match result {
        Ok(hash) => Ok(hash),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context(format!(
            "Failed to get password hash for {} in {}",
            id, slug
        )),
    }
}

/// Update the password hash for a document by ID.
/// Hashes the plaintext password before storing.
pub fn update_password(
    conn: &rusqlite::Connection,
    slug: &str,
    id: &str,
    password: &str,
) -> Result<()> {
    let hash = hash_password(password)?;
    let sql = format!("UPDATE {} SET _password_hash = ?1 WHERE id = ?2", slug);
    conn.execute(&sql, rusqlite::params![hash, id])
        .with_context(|| format!("Failed to update password for {} in {}", id, slug))?;
    Ok(())
}

/// Check whether a user has a password set (non-NULL `_password_hash`).
pub fn has_password(conn: &rusqlite::Connection, slug: &str, id: &str) -> Result<bool> {
    let sql = format!(
        "SELECT _password_hash IS NOT NULL FROM {} WHERE id = ?1",
        slug
    );
    let result = conn.query_row(&sql, [id], |row| row.get::<_, bool>(0));
    match result {
        Ok(v) => Ok(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(e) => Err(e).context(format!("Failed to check password for {} in {}", id, slug)),
    }
}

// ── Reset token functions ─────────────────────────────────────────────────

/// Store a password reset token and expiry for a user.
pub fn set_reset_token(
    conn: &rusqlite::Connection,
    slug: &str,
    user_id: &str,
    token: &str,
    exp: i64,
) -> Result<()> {
    let sql = format!(
        "UPDATE {} SET _reset_token = ?1, _reset_token_exp = ?2 WHERE id = ?3",
        slug
    );
    conn.execute(&sql, rusqlite::params![token, exp, user_id])
        .with_context(|| format!("Failed to set reset token for {} in {}", user_id, slug))?;
    Ok(())
}

/// Find a user by their reset token. Returns the document and token expiry.
pub fn find_by_reset_token(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    token: &str,
) -> Result<Option<(Document, i64)>> {
    let column_names = get_column_names(def);
    let cols = column_names.join(", ");
    let sql = format!(
        "SELECT {}, _reset_token_exp FROM {} WHERE _reset_token = ?1",
        cols, slug
    );

    let result = conn.query_row(&sql, [token], |row| {
        let doc = row_to_document(row, &column_names)?;
        let exp: i64 = row.get(column_names.len())?;
        Ok((doc, exp))
    });

    match result {
        Ok(pair) => Ok(Some(pair)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context(format!("Failed to find user by reset token in {}", slug)),
    }
}

/// Clear the reset token for a user (after successful reset or expiry).
pub fn clear_reset_token(conn: &rusqlite::Connection, slug: &str, user_id: &str) -> Result<()> {
    let sql = format!(
        "UPDATE {} SET _reset_token = NULL, _reset_token_exp = NULL WHERE id = ?1",
        slug
    );
    conn.execute(&sql, [user_id])
        .with_context(|| format!("Failed to clear reset token for {} in {}", user_id, slug))?;
    Ok(())
}

// ── Email verification functions ──────────────────────────────────────────

/// Store a verification token and expiry for a user.
pub fn set_verification_token(
    conn: &rusqlite::Connection,
    slug: &str,
    user_id: &str,
    token: &str,
    exp: i64,
) -> Result<()> {
    let sql = format!(
        "UPDATE {} SET _verification_token = ?1, _verification_token_exp = ?2 WHERE id = ?3",
        slug
    );
    conn.execute(&sql, rusqlite::params![token, exp, user_id])
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
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    token: &str,
) -> Result<Option<(Document, i64)>> {
    let column_names = get_column_names(def);
    let cols = column_names.join(", ");
    let sql = format!(
        "SELECT {}, _verification_token_exp FROM {} WHERE _verification_token = ?1",
        cols, slug
    );

    let result = conn.query_row(&sql, [token], |row| {
        let doc = row_to_document(row, &column_names)?;
        let exp: i64 = row.get(column_names.len()).unwrap_or(0);
        Ok((doc, exp))
    });

    match result {
        Ok(pair) => Ok(Some(pair)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context(format!(
            "Failed to find user by verification token in {}",
            slug
        )),
    }
}

/// Mark a user as verified (set _verified = 1, clear token and expiry).
pub fn mark_verified(conn: &rusqlite::Connection, slug: &str, user_id: &str) -> Result<()> {
    let sql = format!(
        "UPDATE {} SET _verified = 1, _verification_token = NULL, _verification_token_exp = NULL WHERE id = ?1",
        slug
    );
    conn.execute(&sql, [user_id])
        .with_context(|| format!("Failed to mark user {} as verified in {}", user_id, slug))?;
    Ok(())
}

/// Mark a user as unverified (set _verified = 0). Does NOT touch token fields.
pub fn mark_unverified(conn: &rusqlite::Connection, slug: &str, user_id: &str) -> Result<()> {
    let sql = format!("UPDATE {} SET _verified = 0 WHERE id = ?1", slug);
    conn.execute(&sql, [user_id])
        .with_context(|| format!("Failed to mark user {} as unverified in {}", user_id, slug))?;
    Ok(())
}

// ── User settings functions ────────────────────────────────────────────────

/// Get user settings JSON blob from `_crap_user_settings` table.
/// Returns None if no settings saved.
pub fn get_user_settings(conn: &rusqlite::Connection, user_id: &str) -> Result<Option<String>> {
    let result = conn.query_row(
        "SELECT settings FROM _crap_user_settings WHERE user_id = ?1",
        [user_id],
        |row| row.get::<_, String>(0),
    );
    match result {
        Ok(settings) => Ok(Some(settings)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context(format!("Failed to get settings for user {}", user_id)),
    }
}

/// Set user settings JSON blob (UPSERT into `_crap_user_settings`).
pub fn set_user_settings(
    conn: &rusqlite::Connection,
    user_id: &str,
    settings_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO _crap_user_settings (user_id, settings) VALUES (?1, ?2)
         ON CONFLICT(user_id) DO UPDATE SET settings = excluded.settings",
        rusqlite::params![user_id, settings_json],
    )
    .with_context(|| format!("Failed to set settings for user {}", user_id))?;
    Ok(())
}

// ── Lock/unlock functions ─────────────────────────────────────────────────

/// Lock a user account (prevent login).
pub fn lock_user(conn: &rusqlite::Connection, slug: &str, id: &str) -> Result<()> {
    let sql = format!("UPDATE {} SET _locked = 1 WHERE id = ?1", slug);
    conn.execute(&sql, [id])
        .with_context(|| format!("Failed to lock user {} in {}", id, slug))?;
    Ok(())
}

/// Unlock a user account (allow login).
pub fn unlock_user(conn: &rusqlite::Connection, slug: &str, id: &str) -> Result<()> {
    let sql = format!("UPDATE {} SET _locked = 0 WHERE id = ?1", slug);
    conn.execute(&sql, [id])
        .with_context(|| format!("Failed to unlock user {} in {}", id, slug))?;
    Ok(())
}

/// Check if a user account is locked.
pub fn is_locked(conn: &rusqlite::Connection, slug: &str, id: &str) -> Result<bool> {
    let sql = format!("SELECT _locked FROM {} WHERE id = ?1", slug);
    let result = conn.query_row(&sql, [id], |row| row.get::<_, Option<i64>>(0));
    match result {
        Ok(Some(v)) => Ok(v != 0),
        Ok(None) => Ok(false),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(e) => Err(e).context(format!(
            "Failed to check lock status for {} in {}",
            id, slug
        )),
    }
}

/// Check if a user is verified.
pub fn is_verified(conn: &rusqlite::Connection, slug: &str, user_id: &str) -> Result<bool> {
    let sql = format!("SELECT _verified FROM {} WHERE id = ?1", slug);
    let result = conn.query_row(&sql, [user_id], |row| row.get::<_, Option<i64>>(0));
    match result {
        Ok(Some(v)) => Ok(v != 0),
        Ok(None) => Ok(false),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(e) => Err(e).context(format!(
            "Failed to check verification for {} in {}",
            user_id, slug
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;
    use crate::core::field::*;
    use rusqlite::Connection;

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

    fn setup_auth_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
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
        conn
    }

    // ── find_by_email tests ─────────────────────────────────────────────────

    #[test]
    fn find_by_email_found() {
        let conn = setup_auth_db();
        let def = auth_def();
        let result = find_by_email(&conn, "users", &def, "test@example.com").unwrap();
        assert!(result.is_some(), "Should find existing user by email");
        let doc = result.unwrap();
        assert_eq!(doc.id, "user1");
    }

    #[test]
    fn find_by_email_not_found() {
        let conn = setup_auth_db();
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
        let conn = setup_auth_db();
        let result = get_password_hash(&conn, "users", "user1").unwrap();
        assert!(
            result.is_none(),
            "Should return None when no password hash is set"
        );
    }

    #[test]
    fn update_password_then_get() {
        let conn = setup_auth_db();
        update_password(&conn, "users", "user1", "secret123").unwrap();
        let hash = get_password_hash(&conn, "users", "user1").unwrap();
        assert!(hash.is_some(), "Should return Some after setting password");
        let hash_str = hash.unwrap();
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
        let conn = setup_auth_db();
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
        let conn = setup_auth_db();
        let def = auth_def();
        set_reset_token(&conn, "users", "user1", "reset-abc", 9999999999).unwrap();
        let result = find_by_reset_token(&conn, "users", &def, "wrong-token").unwrap();
        assert!(result.is_none(), "Should return None for wrong reset token");
    }

    #[test]
    fn find_by_reset_token_expired() {
        let conn = setup_auth_db();
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
        let conn = setup_auth_db();
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
        let conn = setup_auth_db();
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
        let conn = setup_auth_db();
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
        let conn = setup_auth_db();
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
        let conn = setup_auth_db();
        let result = is_verified(&conn, "users", "user1").unwrap();
        assert!(!result, "Newly created user should not be verified");
    }

    #[test]
    fn mark_verified_then_check() {
        let conn = setup_auth_db();
        mark_verified(&conn, "users", "user1").unwrap();
        let result = is_verified(&conn, "users", "user1").unwrap();
        assert!(result, "User should be verified after mark_verified");
    }

    // ── lock/unlock tests ─────────────────────────────────────────────────

    #[test]
    fn is_locked_default_false() {
        let conn = setup_auth_db();
        let result = is_locked(&conn, "users", "user1").unwrap();
        assert!(!result, "Newly created user should not be locked");
    }

    #[test]
    fn lock_then_check() {
        let conn = setup_auth_db();
        lock_user(&conn, "users", "user1").unwrap();
        let result = is_locked(&conn, "users", "user1").unwrap();
        assert!(result, "User should be locked after lock_user");
    }

    #[test]
    fn lock_then_unlock() {
        let conn = setup_auth_db();
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
        let conn = setup_auth_db();
        let result = is_locked(&conn, "users", "nonexistent").unwrap();
        assert!(!result, "Non-existent user should return false");
    }

    #[test]
    fn is_verified_nonexistent_user() {
        let conn = setup_auth_db();
        let result = is_verified(&conn, "users", "nonexistent").unwrap();
        assert!(!result, "Non-existent user should return false");
    }

    #[test]
    fn mark_unverified_then_check() {
        let conn = setup_auth_db();
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
        let conn = setup_auth_db();
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
    fn mark_verified_clears_token() {
        let conn = setup_auth_db();
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
        let conn = setup_auth_db();
        assert!(!has_password(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn has_password_true_after_set() {
        let conn = setup_auth_db();
        update_password(&conn, "users", "user1", "secret123").unwrap();
        assert!(has_password(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn has_password_nonexistent_user() {
        let conn = setup_auth_db();
        assert!(!has_password(&conn, "users", "nonexistent").unwrap());
    }

    #[test]
    fn get_password_hash_nonexistent_user() {
        let conn = setup_auth_db();
        let result = get_password_hash(&conn, "users", "nonexistent").unwrap();
        assert!(result.is_none(), "Non-existent user should return None");
    }

    #[test]
    fn is_locked_null_value_treated_as_false() {
        let conn = setup_auth_db();
        // user1 has _locked DEFAULT 0 which is an integer, but let's test NULL directly
        conn.execute("UPDATE users SET _locked = NULL WHERE id = 'user1'", [])
            .unwrap();
        let result = is_locked(&conn, "users", "user1").unwrap();
        assert!(!result, "NULL _locked should be treated as false");
    }

    #[test]
    fn is_verified_null_value_treated_as_false() {
        let conn = setup_auth_db();
        conn.execute("UPDATE users SET _verified = NULL WHERE id = 'user1'", [])
            .unwrap();
        let result = is_verified(&conn, "users", "user1").unwrap();
        assert!(!result, "NULL _verified should be treated as false");
    }

    // ── user settings tests (_crap_user_settings table) ──────────────────────

    fn setup_user_settings_table(conn: &Connection) {
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
        let conn = setup_auth_db();
        setup_user_settings_table(&conn);
        let result = get_user_settings(&conn, "user1").unwrap();
        assert!(
            result.is_none(),
            "Should return None when no settings saved"
        );
    }

    #[test]
    fn set_then_get_user_settings() {
        let conn = setup_auth_db();
        setup_user_settings_table(&conn);
        let settings = r#"{"posts":{"columns":["title","status"]}}"#;
        set_user_settings(&conn, "user1", settings).unwrap();
        let result = get_user_settings(&conn, "user1").unwrap();
        assert_eq!(result.as_deref(), Some(settings));
    }

    #[test]
    fn set_user_settings_overwrites() {
        let conn = setup_auth_db();
        setup_user_settings_table(&conn);
        set_user_settings(&conn, "user1", r#"{"a":1}"#).unwrap();
        set_user_settings(&conn, "user1", r#"{"b":2}"#).unwrap();
        let result = get_user_settings(&conn, "user1").unwrap();
        assert_eq!(result.as_deref(), Some(r#"{"b":2}"#));
    }

    #[test]
    fn get_user_settings_nonexistent_user() {
        let conn = setup_auth_db();
        setup_user_settings_table(&conn);
        let result = get_user_settings(&conn, "nonexistent").unwrap();
        assert!(result.is_none(), "Non-existent user should return None");
    }
}
