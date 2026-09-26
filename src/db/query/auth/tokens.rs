//! Reset and verification token lifecycle (set, find, clear, mark verified).

use anyhow::{Context as _, Result};

use crate::{
    core::{Builder, CollectionDefinition, Document, auth::hash_security_value},
    db::{
        DbConnection, DbValue,
        query::{
            LocaleContext,
            helpers::append_soft_delete_filter,
            read::{decode_row, select_columns},
        },
    },
};

/// The column pair one kind of security token lives in.
struct TokenColumns {
    token: &'static str,
    exp: &'static str,
}

const RESET: TokenColumns = TokenColumns {
    token: "_reset_token",
    exp: "_reset_token_exp",
};

const VERIFICATION: TokenColumns = TokenColumns {
    token: "_verification_token",
    exp: "_verification_token_exp",
};

/// A security token to store for one user: which account, the raw token (only
/// its digest is persisted) and its expiry as a Unix timestamp.
#[derive(Builder)]
pub struct TokenGrant<'a> {
    /// The auth collection holding the user.
    #[builder(required)]
    pub slug: &'a str,
    /// The user the token is issued to.
    #[builder(required)]
    pub user_id: &'a str,
    /// The raw token; only its digest is stored.
    #[builder(required)]
    pub token: &'a str,
    /// Expiry as a Unix timestamp (seconds).
    #[builder(required)]
    pub exp: i64,
}

// ── Shared token helpers ─────────────────────────────────────────────────

/// Store a token and expiry in the given columns for a user.
fn set_token(conn: &dyn DbConnection, columns: &TokenColumns, grant: &TokenGrant) -> Result<()> {
    let TokenColumns {
        token: token_col,
        exp: exp_col,
    } = columns;
    let TokenGrant {
        slug,
        user_id,
        token,
        exp,
    } = *grant;
    let (p1, p2, p3) = (
        conn.placeholder(1),
        conn.placeholder(2),
        conn.placeholder(3),
    );
    let sql = format!("UPDATE \"{slug}\" SET {token_col} = {p1}, {exp_col} = {p2} WHERE id = {p3}");

    // The DIGEST is stored, never the token itself — the emailed value stays
    // the only copy of the credential (see `hash_security_value`).
    conn.execute(
        &sql,
        &[
            DbValue::Text(hash_security_value(token)),
            DbValue::Integer(exp),
            DbValue::Text(user_id.to_string()),
        ],
    )
    .with_context(|| format!("Failed to set {token_col} for {user_id} in {slug}"))?;

    Ok(())
}

impl TokenColumns {
    /// Find a user by this token column. Returns the document and token
    /// expiry.
    ///
    /// Reads the locale-aware column list: on an auth collection with
    /// localized fields the bare logical names (`bio`) are not columns
    /// (`bio__en` is), so a bare list fails the SELECT. A trashed user is not
    /// found — a soft-deleted account is disabled and must not consume a reset
    /// or verification link, the same rule the email lookup applies.
    fn find(
        &self,
        conn: &dyn DbConnection,
        def: &CollectionDefinition,
        token: &str,
        locale_ctx: Option<&LocaleContext>,
    ) -> Result<Option<(Document, i64)>> {
        let sql = self.lookup_sql(conn, def, locale_ctx)?;

        // Looked up BY the digest: the column holds hashes, so the presented
        // token is hashed to find its row.
        let Some(row) = conn.query_one(&sql, &[DbValue::Text(hash_security_value(token))])? else {
            return Ok(None);
        };

        let doc = decode_row(conn, &row, &def.fields, locale_ctx)?;
        let exp = row
            .get_i64(self.exp)
            .with_context(|| format!("Failed to read {}", self.exp))?;

        Ok(Some((doc, exp)))
    }

    /// The SELECT finding a live user by this token column.
    fn lookup_sql(
        &self,
        conn: &dyn DbConnection,
        def: &CollectionDefinition,
        locale_ctx: Option<&LocaleContext>,
    ) -> Result<String> {
        let cols = select_columns(def, locale_ctx)?.join(", ");
        let mut sql = format!(
            "SELECT {cols}, {exp} FROM \"{slug}\" WHERE {token} = {p1}",
            exp = self.exp,
            slug = def.slug,
            token = self.token,
            p1 = conn.placeholder(1),
        );

        let mut has_where = true;
        append_soft_delete_filter(def, false, &mut sql, &mut has_where);

        Ok(sql)
    }
}

/// Clear a token and its expiry column for a user.
fn clear_token(
    conn: &dyn DbConnection,
    slug: &str,
    user_id: &str,
    columns: &TokenColumns,
) -> Result<()> {
    let TokenColumns {
        token: token_col,
        exp: exp_col,
    } = columns;
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
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
pub fn set_reset_token(conn: &dyn DbConnection, grant: &TokenGrant) -> Result<()> {
    set_token(conn, &RESET, grant)
}

/// Find a user by their reset token. Returns the document and token expiry.
/// `locale_ctx` must be the default-locale context on a localized auth
/// collection; a trashed user is not found.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails or the row fails to parse.
pub fn find_by_reset_token(
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    token: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Option<(Document, i64)>> {
    RESET.find(conn, def, token, locale_ctx)
}

/// Clear the reset token for a user (after successful reset or expiry).
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
pub fn clear_reset_token(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<()> {
    clear_token(conn, slug, user_id, &RESET)
}

// ── Verification token functions ─────────────────────────────────────────

/// Store a verification token and expiry for a user.
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
pub fn set_verification_token(conn: &dyn DbConnection, grant: &TokenGrant) -> Result<()> {
    set_token(conn, &VERIFICATION, grant)
}

/// Find a user by their verification token. Returns the document and token
/// expiry. `locale_ctx` must be the default-locale context on a localized auth
/// collection; a trashed user is not found.
///
/// # Errors
///
/// Returns a backend error if the SELECT fails or the row fails to parse.
pub fn find_by_verification_token(
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    token: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Option<(Document, i64)>> {
    VERIFICATION.find(conn, def, token, locale_ctx)
}

/// Clear the verification token for a user (after expiry). Does NOT change `_verified` status.
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
pub fn clear_verification_token(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<()> {
    clear_token(conn, slug, user_id, &VERIFICATION)
}

/// Mark a user as verified (set _verified = 1, clear token and expiry).
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
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

/// Mark a user as unverified (set `_verified = 0`) and retire any outstanding
/// verification link.
///
/// The link is cleared because unverifying is an act of revocation: an
/// operator does it when an address looks wrong or compromised. Leaving the
/// original signup link live would let whoever holds it re-verify
/// immediately, anywhere inside its 24-hour window, undoing the revocation.
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
pub fn mark_unverified(conn: &dyn DbConnection, slug: &str, user_id: &str) -> Result<()> {
    let sql = format!(
        "UPDATE \"{slug}\" SET _verified = 0, _verification_token = NULL, \
         _verification_token_exp = NULL WHERE id = {}",
        conn.placeholder(1)
    );
    conn.execute(&sql, &[DbValue::Text(user_id.to_string())])
        .with_context(|| format!("Failed to mark user {user_id} as unverified in {slug}"))?;
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::config::{CrapConfig, LocaleConfig};
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
                id TEXT PRIMARY KEY, _revision INTEGER NOT NULL DEFAULT 0, email TEXT UNIQUE, name TEXT,
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

    /// The `users` table grown a localized `bio` (`bio__en` / `bio__de`), with
    /// the matching definition and default-locale context.
    fn localize(conn: &BoxedConnection) -> (CollectionDefinition, LocaleContext) {
        conn.execute_batch(
            "ALTER TABLE users ADD COLUMN bio__en TEXT;
             ALTER TABLE users ADD COLUMN bio__de TEXT;
             UPDATE users SET bio__en = 'Hello' WHERE id = 'user1';",
        )
        .unwrap();

        let mut def = auth_def();
        def.fields.push(
            FieldDefinition::builder("bio", FieldType::Text)
                .localized(true)
                .build(),
        );

        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };

        (def, LocaleContext::default_for(&config).unwrap())
    }

    /// Regression: the token lookups selected the bare logical column names,
    /// so on an auth collection with a localized field (`bio__en`, no `bio`)
    /// the SELECT failed — breaking the reset page, the reset submit and email
    /// verification.
    #[test]
    fn token_lookups_read_a_localized_auth_collection() {
        let (_dir, conn) = setup();
        let (def, locale_ctx) = localize(&conn);
        set_reset_token(
            &conn,
            &TokenGrant::builder("users", "user1", "reset-l10n", 9_999_999_999).build(),
        )
        .unwrap();
        set_verification_token(
            &conn,
            &TokenGrant::builder("users", "user1", "verify-l10n", 9_999_999_999).build(),
        )
        .unwrap();

        let (doc, _) = find_by_reset_token(&conn, &def, "reset-l10n", Some(&locale_ctx))
            .unwrap()
            .expect("reset token found");
        assert_eq!(doc.id, "user1");
        assert_eq!(doc.get_str("bio"), Some("Hello"));

        let (doc, _) = find_by_verification_token(&conn, &def, "verify-l10n", Some(&locale_ctx))
            .unwrap()
            .expect("verification token found");
        assert_eq!(doc.id, "user1");
    }

    /// Regression: a trashed (soft-deleted) user could still consume a reset
    /// or verification link — the lookup had no soft-delete filter.
    #[test]
    fn token_lookups_skip_a_trashed_user() {
        let (_dir, conn) = setup();
        conn.execute_batch(
            "ALTER TABLE users ADD COLUMN _deleted_at TEXT;
             UPDATE users SET _deleted_at = '2026-01-01T00:00:00Z' WHERE id = 'user1';",
        )
        .unwrap();
        let mut def = auth_def();
        def.soft_delete = true;
        set_reset_token(
            &conn,
            &TokenGrant::builder("users", "user1", "reset-trashed", 9_999_999_999).build(),
        )
        .unwrap();
        set_verification_token(
            &conn,
            &TokenGrant::builder("users", "user1", "verify-trashed", 9_999_999_999).build(),
        )
        .unwrap();

        assert!(
            find_by_reset_token(&conn, &def, "reset-trashed", None)
                .unwrap()
                .is_none()
        );
        assert!(
            find_by_verification_token(&conn, &def, "verify-trashed", None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn set_and_find_reset_token() {
        let (_dir, conn) = setup();
        let def = auth_def();
        set_reset_token(
            &conn,
            &TokenGrant::builder("users", "user1", "reset-abc", 9999999999).build(),
        )
        .unwrap();
        let (doc, exp) = find_by_reset_token(&conn, &def, "reset-abc", None)
            .unwrap()
            .unwrap();
        assert_eq!(doc.id, "user1");
        assert_eq!(exp, 9999999999);
    }

    #[test]
    fn find_by_reset_token_wrong() {
        let (_dir, conn) = setup();
        set_reset_token(
            &conn,
            &TokenGrant::builder("users", "user1", "reset-abc", 9999999999).build(),
        )
        .unwrap();
        assert!(
            find_by_reset_token(&conn, &auth_def(), "wrong-token", None)
                .unwrap()
                .is_none()
        );
    }

    /// The column holds a DIGEST, never the token: a read of the table (or a
    /// backup) inside the token's window must not hand over a usable
    /// credential. Lookup by the raw token still works.
    #[test]
    fn reset_token_is_stored_hashed() {
        let (_dir, conn) = setup();
        set_reset_token(
            &conn,
            &TokenGrant::builder("users", "user1", "plain-token", 9_999_999_999).build(),
        )
        .unwrap();

        let stored = conn
            .query_one("SELECT _reset_token FROM users WHERE id = 'user1'", &[])
            .unwrap()
            .and_then(|r| r.opt_text_at(0))
            .expect("token column");
        assert_ne!(stored, "plain-token", "the raw token must not be stored");
        assert_eq!(stored, hash_security_value("plain-token"));

        assert!(
            find_by_reset_token(&conn, &auth_def(), "plain-token", None)
                .unwrap()
                .is_some(),
            "the raw token still finds its row"
        );
        assert!(
            find_by_reset_token(&conn, &auth_def(), &stored, None)
                .unwrap()
                .is_none(),
            "presenting the stored digest is not presenting the token"
        );
    }

    #[test]
    fn find_by_reset_token_expired() {
        let (_dir, conn) = setup();
        set_reset_token(
            &conn,
            &TokenGrant::builder("users", "user1", "reset-expired", 1000).build(),
        )
        .unwrap();
        let result = find_by_reset_token(&conn, &auth_def(), "reset-expired", None).unwrap();
        assert!(
            result.is_some(),
            "DB returns expired tokens (caller checks)"
        );
        assert_eq!(result.unwrap().1, 1000);
    }

    #[test]
    fn clear_reset_token_works() {
        let (_dir, conn) = setup();
        set_reset_token(
            &conn,
            &TokenGrant::builder("users", "user1", "reset-xyz", 9999999999).build(),
        )
        .unwrap();
        clear_reset_token(&conn, "users", "user1").unwrap();
        assert!(
            find_by_reset_token(&conn, &auth_def(), "reset-xyz", None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn set_and_find_verification_token() {
        let (_dir, conn) = setup();
        set_verification_token(
            &conn,
            &TokenGrant::builder("users", "user1", "verify-abc", 9999999999).build(),
        )
        .unwrap();
        let (doc, exp) = find_by_verification_token(&conn, &auth_def(), "verify-abc", None)
            .unwrap()
            .unwrap();
        assert_eq!(doc.id, "user1");
        assert_eq!(exp, 9999999999);
    }

    #[test]
    fn find_by_verification_token_wrong() {
        let (_dir, conn) = setup();
        set_verification_token(
            &conn,
            &TokenGrant::builder("users", "user1", "verify-abc", 9999999999).build(),
        )
        .unwrap();
        assert!(
            find_by_verification_token(&conn, &auth_def(), "wrong-token", None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn find_by_verification_token_expired() {
        let (_dir, conn) = setup();
        set_verification_token(
            &conn,
            &TokenGrant::builder("users", "user1", "verify-expired", 1000).build(),
        )
        .unwrap();
        let result =
            find_by_verification_token(&conn, &auth_def(), "verify-expired", None).unwrap();
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
        set_verification_token(
            &conn,
            &TokenGrant::builder("users", "user1", "verify-abc", 9999999999).build(),
        )
        .unwrap();
        mark_verified(&conn, "users", "user1").unwrap();
        assert!(
            find_by_verification_token(&conn, &def, "verify-abc", None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn clear_verification_token_does_not_change_verified_status() {
        let (_dir, conn) = setup();
        set_verification_token(
            &conn,
            &TokenGrant::builder("users", "user1", "verify-clear", 9999999999).build(),
        )
        .unwrap();
        clear_verification_token(&conn, "users", "user1").unwrap();
        assert!(!is_verified(&conn, "users", "user1").unwrap());
    }

    #[test]
    fn mark_verified_then_unverify_preserves_cleared_token() {
        let (_dir, conn) = setup();
        let def = auth_def();
        set_verification_token(
            &conn,
            &TokenGrant::builder("users", "user1", "verify-tok", 9999999999).build(),
        )
        .unwrap();
        mark_verified(&conn, "users", "user1").unwrap();
        assert!(
            find_by_verification_token(&conn, &def, "verify-tok", None)
                .unwrap()
                .is_none()
        );
        mark_unverified(&conn, "users", "user1").unwrap();
        assert!(!is_verified(&conn, "users", "user1").unwrap());
    }
}
