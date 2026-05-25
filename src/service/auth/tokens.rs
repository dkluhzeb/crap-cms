//! Password-reset and email-verification token flows.
//!
//! Caller owns the transaction; these functions read + write a
//! single connection. `generate_reset_token` and the verify-side
//! lookups return `Option`/`bool` so admin/gRPC handlers can pick
//! the right HTTP/grpc status (404 vs 410, etc.).

use anyhow::anyhow;
use chrono::Utc;
use nanoid::nanoid;

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

    let Some(user) = query::find_by_email(conn, ctx.slug, def, email)? else {
        return Ok(None);
    };

    let token = nanoid!();
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
/// # Errors
///
/// Returns `InvalidToken` when the token is missing, expired, or
/// the user is locked. Returns a backend error if the DB
/// connection or persistence fails.
pub fn consume_reset_token(
    ctx: &ServiceContext,
    token: &str,
    new_password: &str,
) -> Result<(), ServiceError> {
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

    query::update_password(conn, ctx.slug, &user.id, new_password)?;
    query::clear_reset_token(conn, ctx.slug, &user.id)?;

    Ok(())
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
        let _ = query::clear_verification_token(conn, ctx.slug, &user.id);
        return Ok(false);
    }

    if query::is_locked(conn, ctx.slug, &user.id)? {
        let _ = query::clear_verification_token(conn, ctx.slug, &user.id);
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
        let result = consume_reset_token(&ctx, "tok123", "newpass123");
        assert!(result.is_ok());
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
