//! Password-reset and email-verification token flows.
//!
//! Caller owns the transaction; these functions read + write a
//! single connection. `generate_reset_token` and the verify-side
//! lookups return `Option`/`bool` so admin/gRPC handlers can pick
//! the right HTTP/grpc status (404 vs 410, etc.).

use anyhow::anyhow;
use chrono::Utc;
use nanoid::nanoid;
use tracing::error;

use crate::{
    core::DocumentId,
    db::{
        DbConnection, LocaleContext,
        query::{self, TokenGrant},
    },
    service::{ServiceContext, ServiceError},
};

/// Result of generating a reset token.
pub struct ResetTokenResult {
    pub user_id: DocumentId,
    pub token: String,
}

/// Character length of a single-use security token.
const SECURITY_TOKEN_LEN: usize = 32;

/// Generate a single-use security token (password reset, email verification).
///
/// A 32-character nanoid — the single chokepoint for security-token entropy so
/// no auth flow can drift to a weaker length. Both the reset and the
/// email-verification paths mint their tokens here.
#[must_use]
pub fn generate_security_token() -> String {
    nanoid!(SECURITY_TOKEN_LEN)
}

/// The default-locale context an account lookup reads the user row under.
///
/// Required, not optional: without the locale config a lookup on an auth
/// collection with localized fields would select bare column names (`bio`
/// where only `bio__en` exists) — a context missing it is a wiring bug, so
/// the flow fails closed instead of issuing a query that happens to work only
/// on non-localized collections. `Ok(None)` means localization is disabled.
fn account_locale_ctx(ctx: &ServiceContext) -> Result<Option<LocaleContext>, ServiceError> {
    let Some(config) = ctx.locale_config else {
        return Err(ServiceError::Internal(anyhow!(
            "token flow on `{}` requires the locale config on its service context",
            ctx.slug
        )));
    };

    Ok(LocaleContext::default_for(config))
}

/// Generate a reset token for a user found by email.
///
/// Returns `Ok(None)` if the user is not found — callers should
/// still show "success" to prevent email enumeration.
///
/// # Errors
///
/// Returns an error if the DB connection, collection lookup, or
/// token persistence fails.
pub fn generate_reset_token(
    ctx: &ServiceContext,
    email: &str,
    expiry_secs: u64,
) -> Result<Option<ResetTokenResult>, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;

    // A soft-deleted account is disabled: don't issue a reset token for a trashed
    // user (consistent with login and the per-request evaluator).
    let locale_ctx = account_locale_ctx(ctx)?;
    let Some(user) = query::find_by_email(conn, ctx.slug, def, email, false, locale_ctx.as_ref())?
    else {
        return Ok(None);
    };

    let token = generate_security_token();
    // Reject the impossible overflow rather than producing an
    // `i64::MAX` / immediate-expiry token; both directions are
    // wrong for a reset token.
    let expiry_i64 = i64::try_from(expiry_secs)
        .map_err(|_| ServiceError::Internal(anyhow!("expiry_secs exceeds i64::MAX")))?;
    let exp = Utc::now().timestamp() + expiry_i64;

    query::set_reset_token(
        conn,
        &TokenGrant::builder(ctx.slug, &user.id, &token, exp).build(),
    )?;

    Ok(Some(ResetTokenResult {
        user_id: user.id,
        token,
    }))
}

/// How long an email-verification link stays valid: 24 hours.
///
/// One constant for the sign-up email and every resend, so a resent link
/// never outlives or undercuts the original.
pub const VERIFICATION_TOKEN_EXPIRY: u64 = 86_400;

/// Mint a verification token for a known account and store it.
///
/// Replaces any outstanding token, so the newest link is the only live one
/// and a resend can't leave a second valid token in circulation. Returns the
/// token to put in the link.
///
/// # Errors
///
/// Returns an error if the expiry overflows or persistence fails.
pub fn issue_verification_token(
    conn: &dyn DbConnection,
    slug: &str,
    user_id: &str,
    expiry_secs: u64,
) -> Result<String, ServiceError> {
    let token = generate_security_token();
    let expiry_i64 = i64::try_from(expiry_secs)
        .map_err(|_| ServiceError::Internal(anyhow!("expiry_secs exceeds i64::MAX")))?;
    let exp = Utc::now().timestamp() + expiry_i64;

    query::set_verification_token(
        conn,
        &TokenGrant::builder(slug, user_id, &token, exp).build(),
    )?;

    Ok(token)
}

/// Result of issuing an email-verification token.
pub struct VerificationTokenResult {
    pub user_id: DocumentId,
    /// The address stored on the account, not the one the caller typed —
    /// the email goes to the account, never to whatever the request supplied.
    pub email: String,
    pub token: String,
}

/// Issue a fresh email-verification token for the account with `email`.
///
/// `Ok(None)` when there is nothing to send: no such account, it is already
/// verified, it is locked, or it has no address to send to. Callers must
/// answer identically in every case so the endpoint never confirms which
/// addresses are registered.
///
/// # Errors
///
/// Returns an error if the DB connection, collection lookup, or token
/// persistence fails.
pub fn generate_verification_token(
    ctx: &ServiceContext,
    email: &str,
    expiry_secs: u64,
) -> Result<Option<VerificationTokenResult>, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;

    // A soft-deleted account is disabled — same rule as the reset flow.
    let locale_ctx = account_locale_ctx(ctx)?;
    let Some(user) = query::find_by_email(conn, ctx.slug, def, email, false, locale_ctx.as_ref())?
    else {
        return Ok(None);
    };

    if query::is_locked(conn, ctx.slug, &user.id)? || query::is_verified(conn, ctx.slug, &user.id)?
    {
        return Ok(None);
    }

    let Some(stored_email) = user.get_str("email").map(str::to_string) else {
        return Ok(None);
    };

    let token = issue_verification_token(conn, ctx.slug, &user.id, expiry_secs)?;

    Ok(Some(VerificationTokenResult {
        user_id: user.id,
        email: stored_email,
        token,
    }))
}

/// Validate a reset token and update the user's password (which clears the
/// token in the same statement).
///
/// Writes nothing on a refusal. Every surface reaches this through
/// [`reset_password_with_token`](super::reset_password_with_token), which owns
/// the transaction and rolls back on any failure.
///
/// On success returns the affected user's id so the caller can tear down that
/// user's live-update streams **after committing** (a reset is a
/// session-revoking action; the version bump only blocks new requests, while an
/// open stream must be invalidated to drop). Publishing is the caller's job —
/// post-commit — so a rolled-back reset never spuriously tears down a stream.
///
/// # Errors
///
/// Returns `InvalidToken` when the token is missing, expired, or
/// the user is locked. Returns a backend error if the DB
/// connection or persistence fails.
pub(super) fn consume_reset_token(
    ctx: &ServiceContext,
    token: &str,
    new_password: &str,
) -> Result<String, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;

    let locale_ctx = account_locale_ctx(ctx)?;
    let (user, exp) = query::find_by_reset_token(conn, def, token, locale_ctx.as_ref())?.ok_or(
        ServiceError::InvalidToken {
            kind: "reset",
            reason: "not found",
        },
    )?;

    if query::is_locked(conn, ctx.slug, &user.id)? {
        return Err(ServiceError::InvalidToken {
            kind: "reset",
            reason: "not found",
        });
    }

    if Utc::now().timestamp() >= exp {
        return Err(ServiceError::InvalidToken {
            kind: "reset",
            reason: "expired",
        });
    }

    // One statement: password change + token clear must be atomic so a
    // mid-flow failure can never leave the consumed token alive after the
    // password actually changed.
    query::update_password(conn, ctx.slug, &user.id, new_password)?;

    // A password reset bumps `_session_version` (killing old JWTs on their next
    // request). Tearing down the user's open live-update streams (which never
    // re-request) is the caller's job, POST-COMMIT — return the id for it.
    Ok(user.id.to_string())
}

/// Validate a verification token and mark the user as verified.
///
/// Returns `true` if the token was valid and the user was marked
/// verified. Returns `false` if the token was not found or expired
/// (caller shows generic message). Clears expired tokens. Caller
/// manages the transaction.
///
/// # Errors
///
/// Returns a backend error if the DB connection, collection
/// lookup, or persistence calls fail. Expired tokens and missing
/// users are not errors.
pub fn consume_verification_token(ctx: &ServiceContext, token: &str) -> Result<bool, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;

    let locale_ctx = account_locale_ctx(ctx)?;
    let Some((user, exp)) =
        query::find_by_verification_token(conn, def, token, locale_ctx.as_ref())?
    else {
        return Ok(false);
    };

    if Utc::now().timestamp() >= exp {
        let _ = query::clear_verification_token(conn, ctx.slug, &user.id)
            .inspect_err(|e| error!("failed to clear expired verification token: {e:#}"));
        return Ok(false);
    }

    if query::is_locked(conn, ctx.slug, &user.id)? {
        let _ = query::clear_verification_token(conn, ctx.slug, &user.id)
            .inspect_err(|e| error!("failed to clear verification token for locked user: {e:#}"));
        return Ok(false);
    }

    query::mark_verified(conn, ctx.slug, &user.id)?;

    Ok(true)
}

/// Validate a password reset token without consuming it (for
/// rendering the reset page).
///
/// # Errors
///
/// Returns a backend error if the DB connection, collection
/// lookup, or query fails.
pub fn find_by_reset_token(ctx: &ServiceContext, token: &str) -> Result<bool, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;

    // Expiry counts here too: the reset PAGE uses this to decide whether to
    // render the form. Ignoring it showed the form for a dead link and only
    // failed on submit, after the user had typed a new password.
    let locale_ctx = account_locale_ctx(ctx)?;

    Ok(
        query::find_by_reset_token(conn, def, token, locale_ctx.as_ref())?
            .is_some_and(|(_, exp)| Utc::now().timestamp() < exp),
    )
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::sync::LazyLock;

    use super::*;
    use crate::{
        config::LocaleConfig,
        core::{CollectionDefinition, FieldDefinition, FieldType},
        service::auth::test_support::setup,
    };

    /// Localization disabled — the config every non-localized test runs under.
    static NO_LOCALES: LazyLock<LocaleConfig> = LazyLock::new(LocaleConfig::default);

    /// The `users` context the token flows run under.
    fn users_ctx<'a>(
        conn: &'a dyn DbConnection,
        def: &'a CollectionDefinition,
    ) -> ServiceContext<'a> {
        ServiceContext::collection("users", def)
            .conn(conn)
            .locale_config(Some(&*NO_LOCALES))
            .build()
    }

    /// Regression: a context built without the locale config silently read
    /// the user row under bare column names — right only by accident on a
    /// non-localized collection. Every token flow now refuses such a context.
    #[test]
    fn a_context_without_locale_config_fails_closed() {
        let (conn, def, _) = setup();
        let exp = Utc::now().timestamp() + 3600;
        query::set_reset_token(
            &conn,
            &TokenGrant::builder("users", "u1", "rtok", exp).build(),
        )
        .unwrap();
        query::set_verification_token(
            &conn,
            &TokenGrant::builder("users", "u1", "vtok", exp).build(),
        )
        .unwrap();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        let internal = |r: Result<_, ServiceError>| matches!(r, Err(ServiceError::Internal(_)));

        assert!(internal(
            generate_reset_token(&ctx, "test@example.com", 3600).map(|_| ())
        ));
        assert!(internal(
            generate_verification_token(&ctx, "test@example.com", 3600).map(|_| ())
        ));
        assert!(internal(find_by_reset_token(&ctx, "rtok").map(|_| ())));
        assert!(internal(
            consume_verification_token(&ctx, "vtok").map(|_| ())
        ));
        assert!(internal(
            consume_reset_token(&ctx, "rtok", "newpass123").map(|_| ())
        ));

        // Nothing was consumed: the tokens still work under a complete context.
        assert!(find_by_reset_token(&users_ctx(&conn, &def), "rtok").unwrap());
    }

    fn two_locales() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    /// Regression: on an auth collection with a localized field the token
    /// lookups selected a bare `bio` column that doesn't exist (`bio__en`
    /// does), so the reset page, the reset submit and email verification all
    /// failed.
    #[test]
    fn token_flows_work_on_a_localized_auth_collection() {
        let (conn, mut def, _) = setup();
        conn.execute_batch(
            "ALTER TABLE users ADD COLUMN bio__en TEXT;
             ALTER TABLE users ADD COLUMN bio__de TEXT;
             UPDATE users SET _verified = 0 WHERE id = 'u1';",
        )
        .unwrap();
        def.fields.push(
            FieldDefinition::builder("bio", FieldType::Text)
                .localized(true)
                .build(),
        );
        let locales = two_locales();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .locale_config(Some(&locales))
            .build();
        let exp = Utc::now().timestamp() + 3600;

        query::set_verification_token(
            &conn,
            &TokenGrant::builder("users", "u1", "vtok", exp).build(),
        )
        .unwrap();
        assert!(consume_verification_token(&ctx, "vtok").unwrap());

        query::set_reset_token(
            &conn,
            &TokenGrant::builder("users", "u1", "rtok", exp).build(),
        )
        .unwrap();
        assert!(find_by_reset_token(&ctx, "rtok").unwrap());
        assert_eq!(
            consume_reset_token(&ctx, "rtok", "newpass123").unwrap(),
            "u1"
        );
    }

    /// Regression: a trashed user could consume a reset link.
    #[test]
    fn a_trashed_user_cannot_consume_a_reset_link() {
        let (conn, mut def, _) = setup();
        conn.execute_batch(
            "ALTER TABLE users ADD COLUMN _deleted_at TEXT;
             UPDATE users SET _deleted_at = '2026-01-01T00:00:00Z' WHERE id = 'u1';",
        )
        .unwrap();
        def.soft_delete = true;
        let ctx = users_ctx(&conn, &def);
        let exp = Utc::now().timestamp() + 3600;
        query::set_reset_token(
            &conn,
            &TokenGrant::builder("users", "u1", "rtok", exp).build(),
        )
        .unwrap();

        assert!(!find_by_reset_token(&ctx, "rtok").unwrap());
        assert!(matches!(
            consume_reset_token(&ctx, "rtok", "newpass123"),
            Err(ServiceError::InvalidToken {
                reason: "not found",
                ..
            })
        ));
    }

    #[test]
    fn security_token_is_32_chars() {
        // Regression: the reset flow used to mint a 21-char default nanoid while
        // the verification flow used 32 — the two security tokens drifted. Both
        // now share `generate_security_token`, pinned to 32 chars.
        assert_eq!(generate_security_token().chars().count(), 32);
    }

    #[test]
    fn generate_reset_token_success() {
        let (conn, def, _) = setup();
        let ctx = users_ctx(&conn, &def);
        let result = generate_reset_token(&ctx, "test@example.com", 3600).unwrap();
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.user_id, "u1");
        assert!(!r.token.is_empty());
    }

    /// The reset PAGE asks this before rendering the form, so an expired
    /// token must read as invalid here — otherwise the user types a new
    /// password into a form that can only fail.
    #[test]
    fn find_by_reset_token_rejects_an_expired_token() {
        let (conn, def, _) = setup();
        let ctx = users_ctx(&conn, &def);

        query::set_reset_token(
            &conn,
            &TokenGrant::builder("users", "u1", "live-token", Utc::now().timestamp() + 600).build(),
        )
        .unwrap();
        assert!(find_by_reset_token(&ctx, "live-token").unwrap());

        query::set_reset_token(
            &conn,
            &TokenGrant::builder("users", "u1", "dead-token", Utc::now().timestamp() - 1).build(),
        )
        .unwrap();
        assert!(
            !find_by_reset_token(&ctx, "dead-token").unwrap(),
            "an expired token is not a valid reset token"
        );
    }

    /// A password change invalidates an outstanding reset link: the token is
    /// cleared by the same statement that writes the hash.
    #[test]
    fn changing_the_password_clears_a_pending_reset_token() {
        let (conn, def, _) = setup();
        let ctx = users_ctx(&conn, &def);

        query::set_reset_token(
            &conn,
            &TokenGrant::builder("users", "u1", "pending", Utc::now().timestamp() + 600).build(),
        )
        .unwrap();
        query::update_password(&conn, "users", "u1", "brand-new-password").unwrap();

        assert!(
            !find_by_reset_token(&ctx, "pending").unwrap(),
            "the emailed reset link must not survive a password change"
        );
    }

    #[test]
    fn generate_reset_token_user_not_found() {
        let (conn, def, _) = setup();
        let ctx = users_ctx(&conn, &def);
        let result = generate_reset_token(&ctx, "nobody@example.com", 3600).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn consume_reset_token_success() {
        let (conn, def, _) = setup();
        let exp = Utc::now().timestamp() + 3600;
        query::set_reset_token(
            &conn,
            &TokenGrant::builder("users", "u1", "tok123", exp).build(),
        )
        .unwrap();

        let ctx = users_ctx(&conn, &def);
        let user_id = consume_reset_token(&ctx, "tok123", "newpass123").expect("reset succeeds");
        assert_eq!(
            user_id, "u1",
            "consume_reset_token returns the affected user id for post-commit teardown"
        );
    }

    /// Regression: the token must be cleared in the SAME statement as the
    /// password update (it used to be a second UPDATE — a mid-flow failure
    /// could leave the consumed token alive). Single-use: the second consume
    /// with the same token must fail.
    #[test]
    fn consume_reset_token_is_single_use() {
        let (conn, def, _) = setup();
        let exp = Utc::now().timestamp() + 3600;
        query::set_reset_token(
            &conn,
            &TokenGrant::builder("users", "u1", "tok-once", exp).build(),
        )
        .unwrap();

        let ctx = users_ctx(&conn, &def);
        consume_reset_token(&ctx, "tok-once", "newpass123").expect("first consume succeeds");

        let err = consume_reset_token(&ctx, "tok-once", "otherpass456")
            .expect_err("second consume with the same token must fail");
        assert!(
            matches!(err, ServiceError::InvalidToken { .. }),
            "expected InvalidToken, got: {err:?}"
        );
    }

    #[test]
    fn consume_reset_token_not_found() {
        let (conn, def, _) = setup();
        let ctx = users_ctx(&conn, &def);
        let result = consume_reset_token(&ctx, "invalid", "newpass123");
        assert!(matches!(
            result,
            Err(ServiceError::InvalidToken {
                kind: "reset",
                reason: "not found"
            })
        ));
    }

    #[test]
    fn consume_reset_token_expired() {
        let (conn, def, _) = setup();
        let exp = Utc::now().timestamp() - 100;
        query::set_reset_token(
            &conn,
            &TokenGrant::builder("users", "u1", "tok123", exp).build(),
        )
        .unwrap();

        let ctx = users_ctx(&conn, &def);
        let result = consume_reset_token(&ctx, "tok123", "newpass123");
        assert!(matches!(
            result,
            Err(ServiceError::InvalidToken {
                kind: "reset",
                reason: "expired"
            })
        ));
    }

    #[test]
    fn consume_reset_token_locked() {
        let (conn, def, _) = setup();
        let exp = Utc::now().timestamp() + 3600;
        query::set_reset_token(
            &conn,
            &TokenGrant::builder("users", "u1", "tok123", exp).build(),
        )
        .unwrap();
        conn.execute("UPDATE users SET _locked = 1 WHERE id = 'u1'", [])
            .unwrap();

        let ctx = users_ctx(&conn, &def);
        let result = consume_reset_token(&ctx, "tok123", "newpass123");
        assert!(matches!(
            result,
            Err(ServiceError::InvalidToken { kind: "reset", .. })
        ));
    }

    #[test]
    fn consume_verification_token_success() {
        let (conn, def, _) = setup();
        let exp = Utc::now().timestamp() + 3600;
        query::set_verification_token(
            &conn,
            &TokenGrant::builder("users", "u1", "vtok", exp).build(),
        )
        .unwrap();
        conn.execute("UPDATE users SET _verified = 0 WHERE id = 'u1'", [])
            .unwrap();

        let ctx = users_ctx(&conn, &def);
        let result = consume_verification_token(&ctx, "vtok").unwrap();
        assert!(result);
    }

    #[test]
    fn consume_verification_token_not_found() {
        let (conn, def, _) = setup();
        let ctx = users_ctx(&conn, &def);
        let result = consume_verification_token(&ctx, "invalid").unwrap();
        assert!(!result);
    }

    #[test]
    fn consume_verification_token_expired() {
        let (conn, def, _) = setup();
        let exp = Utc::now().timestamp() - 100;
        query::set_verification_token(
            &conn,
            &TokenGrant::builder("users", "u1", "vtok", exp).build(),
        )
        .unwrap();

        let ctx = users_ctx(&conn, &def);
        let result = consume_verification_token(&ctx, "vtok").unwrap();
        assert!(!result);
    }

    #[test]
    fn consume_verification_token_locked() {
        let (conn, def, _) = setup();
        let exp = Utc::now().timestamp() + 3600;
        query::set_verification_token(
            &conn,
            &TokenGrant::builder("users", "u1", "vtok", exp).build(),
        )
        .unwrap();
        conn.execute("UPDATE users SET _locked = 1 WHERE id = 'u1'", [])
            .unwrap();

        let ctx = users_ctx(&conn, &def);
        let result = consume_verification_token(&ctx, "vtok").unwrap();
        assert!(!result);
    }

    /// The happy path: an unverified account gets a token it can then spend,
    /// and the email goes to the address on record.
    #[test]
    fn generate_verification_token_issues_a_spendable_token() {
        let (conn, def, _) = setup();
        conn.execute("UPDATE users SET _verified = 0 WHERE id = 'u1'", [])
            .unwrap();

        let ctx = users_ctx(&conn, &def);
        let issued = generate_verification_token(&ctx, "test@example.com", 3600)
            .unwrap()
            .expect("an unverified account gets a token");

        assert_eq!(issued.user_id.to_string(), "u1");
        assert_eq!(issued.email, "test@example.com");
        assert_eq!(issued.token.chars().count(), 32);

        assert!(consume_verification_token(&ctx, &issued.token).unwrap());
    }

    /// The address on the account is what gets mailed, not the spelling the
    /// caller typed — a lookup is case-insensitive, delivery is not.
    #[test]
    fn generate_verification_token_returns_the_stored_address() {
        let (conn, def, _) = setup();
        conn.execute("UPDATE users SET _verified = 0 WHERE id = 'u1'", [])
            .unwrap();

        let ctx = users_ctx(&conn, &def);
        let issued = generate_verification_token(&ctx, "TEST@Example.COM", 3600)
            .unwrap()
            .expect("the lookup is case-insensitive");

        assert_eq!(issued.email, "test@example.com");
    }

    /// A resend replaces the outstanding token rather than adding a second
    /// live one, so an old link in an old inbox stops working.
    #[test]
    fn a_reissued_token_retires_the_previous_one() {
        let (conn, def, _) = setup();
        conn.execute("UPDATE users SET _verified = 0 WHERE id = 'u1'", [])
            .unwrap();

        let ctx = users_ctx(&conn, &def);
        let first = generate_verification_token(&ctx, "test@example.com", 3600)
            .unwrap()
            .unwrap();
        let second = generate_verification_token(&ctx, "test@example.com", 3600)
            .unwrap()
            .unwrap();

        assert_ne!(first.token, second.token);
        assert!(
            !consume_verification_token(&ctx, &first.token).unwrap(),
            "the superseded link must be dead"
        );
        assert!(consume_verification_token(&ctx, &second.token).unwrap());
    }

    /// Nothing to send: no such address, already verified, or locked. All
    /// three answer `None` so the caller's response cannot tell them apart.
    #[test]
    fn generate_verification_token_declines_silently() {
        let (conn, def, _) = setup();
        let ctx = users_ctx(&conn, &def);

        // Seeded as verified.
        assert!(
            generate_verification_token(&ctx, "test@example.com", 3600)
                .unwrap()
                .is_none()
        );

        assert!(
            generate_verification_token(&ctx, "nobody@example.com", 3600)
                .unwrap()
                .is_none()
        );

        conn.execute(
            "UPDATE users SET _verified = 0, _locked = 1 WHERE id = 'u1'",
            [],
        )
        .unwrap();
        assert!(
            generate_verification_token(&ctx, "test@example.com", 3600)
                .unwrap()
                .is_none()
        );
    }

    /// A declined resend leaves any existing token untouched — it must not
    /// clear the link a legitimate sign-up email already carried.
    #[test]
    fn a_declined_resend_does_not_disturb_an_existing_token() {
        let (conn, def, _) = setup();
        let exp = Utc::now().timestamp() + 3600;
        query::set_verification_token(
            &conn,
            &TokenGrant::builder("users", "u1", "vtok", exp).build(),
        )
        .unwrap();

        let ctx = users_ctx(&conn, &def);
        // The seeded user is verified, so the resend declines.
        assert!(
            generate_verification_token(&ctx, "test@example.com", 3600)
                .unwrap()
                .is_none()
        );

        conn.execute("UPDATE users SET _verified = 0 WHERE id = 'u1'", [])
            .unwrap();
        assert!(consume_verification_token(&ctx, "vtok").unwrap());
    }
}
