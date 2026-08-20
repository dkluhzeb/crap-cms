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
    db::query,
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
    let Some(user) = query::find_by_email(conn, ctx.slug, def, email, false)? else {
        return Ok(None);
    };

    let token = generate_security_token();
    // Reject the impossible overflow rather than producing an
    // `i64::MAX` / immediate-expiry token; both directions are
    // wrong for a reset token.
    let expiry_i64 = i64::try_from(expiry_secs)
        .map_err(|_| ServiceError::Internal(anyhow!("expiry_secs exceeds i64::MAX")))?;
    let exp = Utc::now().timestamp() + expiry_i64;

    query::set_reset_token(conn, ctx.slug, &user.id, &token, exp)?;

    Ok(Some(ResetTokenResult {
        user_id: user.id,
        token,
    }))
}

/// Validate a reset token and update the user's password.
///
/// Clears the token on success or if it's expired/locked. Caller
/// manages the transaction.
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
pub fn consume_reset_token(
    ctx: &ServiceContext,
    token: &str,
    new_password: &str,
) -> Result<String, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;

    let (user, exp) = query::find_by_reset_token(conn, ctx.slug, def, token)?.ok_or(
        ServiceError::InvalidToken {
            kind: "reset",
            reason: "not found",
        },
    )?;

    if query::is_locked(conn, ctx.slug, &user.id)? {
        query::clear_reset_token(conn, ctx.slug, &user.id)?;
        return Err(ServiceError::InvalidToken {
            kind: "reset",
            reason: "not found",
        });
    }

    if Utc::now().timestamp() >= exp {
        query::clear_reset_token(conn, ctx.slug, &user.id)?;
        return Err(ServiceError::InvalidToken {
            kind: "reset",
            reason: "expired",
        });
    }

    // One statement: password change + token clear must be atomic so a
    // mid-flow failure can never leave the consumed token alive after the
    // password actually changed.
    query::update_password_clearing_reset_token(conn, ctx.slug, &user.id, new_password)?;

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

    let Some((user, exp)) = query::find_by_verification_token(conn, ctx.slug, def, token)? else {
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

    Ok(query::find_by_reset_token(conn, ctx.slug, def, token)?.is_some())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::service::auth::test_support::setup;

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
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
        let result = generate_reset_token(&ctx, "test@example.com", 3600).unwrap();
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.user_id, "u1");
        assert!(!r.token.is_empty());
    }

    #[test]
    fn generate_reset_token_user_not_found() {
        let (conn, def, _) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
        let result = generate_reset_token(&ctx, "nobody@example.com", 3600).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn consume_reset_token_success() {
        let (conn, def, _) = setup();
        let exp = Utc::now().timestamp() + 3600;
        conn.execute(
            "UPDATE users SET _reset_token = 'tok123', _reset_token_exp = ?1 WHERE id = 'u1'",
            [exp],
        )
        .unwrap();

        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
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
        conn.execute(
            "UPDATE users SET _reset_token = 'tok-once', _reset_token_exp = ?1 WHERE id = 'u1'",
            [exp],
        )
        .unwrap();

        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
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
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
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
        conn.execute(
            "UPDATE users SET _reset_token = 'tok123', _reset_token_exp = ?1 WHERE id = 'u1'",
            [exp],
        )
        .unwrap();

        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
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
        conn.execute(
            "UPDATE users SET _reset_token = 'tok123', _reset_token_exp = ?1, _locked = 1 WHERE id = 'u1'",
            [exp],
        ).unwrap();

        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
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
        conn.execute(
            "UPDATE users SET _verification_token = 'vtok', _verification_token_exp = ?1, _verified = 0 WHERE id = 'u1'",
            [exp],
        ).unwrap();

        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
        let result = consume_verification_token(&ctx, "vtok").unwrap();
        assert!(result);
    }

    #[test]
    fn consume_verification_token_not_found() {
        let (conn, def, _) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
        let result = consume_verification_token(&ctx, "invalid").unwrap();
        assert!(!result);
    }

    #[test]
    fn consume_verification_token_expired() {
        let (conn, def, _) = setup();
        let exp = Utc::now().timestamp() - 100;
        conn.execute(
            "UPDATE users SET _verification_token = 'vtok', _verification_token_exp = ?1 WHERE id = 'u1'",
            [exp],
        ).unwrap();

        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
        let result = consume_verification_token(&ctx, "vtok").unwrap();
        assert!(!result);
    }

    #[test]
    fn consume_verification_token_locked() {
        let (conn, def, _) = setup();
        let exp = Utc::now().timestamp() + 3600;
        conn.execute(
            "UPDATE users SET _verification_token = 'vtok', _verification_token_exp = ?1, _locked = 1 WHERE id = 'u1'",
            [exp],
        ).unwrap();

        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
        let result = consume_verification_token(&ctx, "vtok").unwrap();
        assert!(!result);
    }
}
