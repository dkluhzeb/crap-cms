//! Auth service layer — composite authentication operations.
//!
//! Consolidates auth flows shared between admin handlers and gRPC handlers:
//! local authentication, password reset tokens, and email verification tokens.
//! Callers still own rate limiting, MFA, auth strategies, token/session creation,
//! and response formatting.

use chrono::Utc;
use nanoid::nanoid;

use crate::{
    core::{Document, DocumentId, auth::PasswordProvider},
    db::query,
    service::{ServiceContext, ServiceError},
};

/// Result of a successful local authentication.
pub struct AuthResult {
    pub user: Document,
    pub session_version: u64,
}

/// Result of generating a reset token.
pub struct ResetTokenResult {
    pub user_id: DocumentId,
    pub token: String,
}

/// Authenticate a user by email and password.
///
/// Performs: find_by_email → verify_password → check_locked → check_verified → session_version.
/// Returns `InvalidCredentials` if the user is not found or the password is wrong.
/// Does NOT handle rate limiting, MFA, auth strategies, or token creation — those
/// are surface concerns.
pub fn authenticate_local(
    ctx: &ServiceContext,
    email: &str,
    password: &str,
    password_provider: &dyn PasswordProvider,
    require_verified: bool,
) -> Result<AuthResult, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def();

    let user = match query::find_by_email(conn, ctx.slug, def, email)? {
        Some(u) => u,
        None => {
            password_provider.dummy_verify();
            return Err(ServiceError::InvalidCredentials);
        }
    };

    let verified = match query::get_password_hash(conn, ctx.slug, &user.id)? {
        Some(hash) => password_provider.verify_password(password, hash.as_ref())?,
        None => false,
    };

    if !verified {
        return Err(ServiceError::InvalidCredentials);
    }

    if query::is_locked(conn, ctx.slug, &user.id)? {
        return Err(ServiceError::AccountLocked);
    }

    if require_verified && !query::is_verified(conn, ctx.slug, &user.id)? {
        return Err(ServiceError::EmailNotVerified);
    }

    let session_version = query::get_session_version(conn, ctx.slug, &user.id)?;

    Ok(AuthResult {
        user,
        session_version,
    })
}

/// Generate a reset token for a user found by email.
///
/// Returns `Ok(None)` if the user is not found — callers should still show "success"
/// to prevent email enumeration.
pub fn generate_reset_token(
    ctx: &ServiceContext,
    email: &str,
    expiry_secs: u64,
) -> Result<Option<ResetTokenResult>, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def();

    let user = match query::find_by_email(conn, ctx.slug, def, email)? {
        Some(u) => u,
        None => return Ok(None),
    };

    let token = nanoid!();
    let exp = Utc::now().timestamp() + expiry_secs as i64;

    query::set_reset_token(conn, ctx.slug, &user.id, &token, exp)?;

    Ok(Some(ResetTokenResult {
        user_id: user.id,
        token,
    }))
}

/// Validate a reset token and update the user's password.
///
/// Clears the token on success or if it's expired/locked. Caller manages the transaction.
pub fn consume_reset_token(
    ctx: &ServiceContext,
    token: &str,
    new_password: &str,
) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def();

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
/// Returns `true` if the token was valid and the user was marked verified.
/// Returns `false` if the token was not found or expired (caller shows generic message).
/// Clears expired tokens. Caller manages the transaction.
pub fn consume_verification_token(ctx: &ServiceContext, token: &str) -> Result<bool, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def();

    let (user, exp) = match query::find_by_verification_token(conn, ctx.slug, def, token)? {
        Some(pair) => pair,
        None => return Ok(false),
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

// ── Account status operations ───────────────────────────────────────

/// Lock a user account, preventing login.
///
/// Publishes a user-invalidation signal (if a transport is configured on
/// the context) so any active live-update streams owned by this user are
/// torn down. Publish is a no-op when no transport is attached.
pub fn lock_user(ctx: &ServiceContext, id: &str) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    query::lock_user(conn.as_ref(), ctx.slug, id)?;

    // Tear down any live-update streams owned by this user. No-op when no
    // transport is attached to the context.
    ctx.publish_user_invalidation(id);

    Ok(())
}

/// Unlock a user account.
pub fn unlock_user(ctx: &ServiceContext, id: &str) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    query::unlock_user(conn.as_ref(), ctx.slug, id)?;

    Ok(())
}

/// Mark a user's email as verified.
pub fn mark_verified(ctx: &ServiceContext, id: &str) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    query::mark_verified(conn.as_ref(), ctx.slug, id)?;

    Ok(())
}

/// Mark a user's email as unverified.
pub fn mark_unverified(ctx: &ServiceContext, id: &str) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    query::mark_unverified(conn.as_ref(), ctx.slug, id)?;

    Ok(())
}

/// Check whether a user account is locked.
pub fn is_locked(ctx: &ServiceContext, id: &str) -> Result<bool, ServiceError> {
    let conn = ctx.resolve_conn()?;
    Ok(query::is_locked(conn.as_ref(), ctx.slug, id)?)
}

/// Check whether a user's email is verified.
pub fn is_verified(ctx: &ServiceContext, id: &str) -> Result<bool, ServiceError> {
    let conn = ctx.resolve_conn()?;
    Ok(query::is_verified(conn.as_ref(), ctx.slug, id)?)
}

/// Get the current session version for a user (for JWT invalidation).
pub fn get_session_version(ctx: &ServiceContext, id: &str) -> Result<u64, ServiceError> {
    let conn = ctx.resolve_conn()?;
    Ok(query::get_session_version(conn.as_ref(), ctx.slug, id)?)
}

/// Check whether a user document exists.
pub fn user_exists(ctx: &ServiceContext, id: &str) -> Result<bool, ServiceError> {
    let conn = ctx.resolve_conn()?;
    Ok(query::user_exists(conn.as_ref(), ctx.slug, id)?)
}

/// Validate a password reset token without consuming it (for rendering the reset page).
pub fn find_by_reset_token(ctx: &ServiceContext, token: &str) -> Result<bool, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def();

    Ok(query::find_by_reset_token(conn, ctx.slug, def, token)?.is_some())
}

/// Store an MFA code for a user.
pub fn set_mfa_code(
    ctx: &ServiceContext,
    id: &str,
    code: &str,
    expiry: i64,
) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    query::set_mfa_code(conn.as_ref(), ctx.slug, id, code, expiry)?;

    Ok(())
}

/// Verify an MFA code. Returns true if valid and not expired.
pub fn verify_mfa_code(ctx: &ServiceContext, id: &str, code: &str) -> Result<bool, ServiceError> {
    let conn = ctx.resolve_conn()?;
    Ok(query::verify_mfa_code(conn.as_ref(), ctx.slug, id, code)?)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rusqlite::Connection;

    use crate::{
        core::{
            CollectionDefinition, FieldDefinition,
            auth::{Argon2PasswordProvider, PasswordProvider},
            collection::Auth,
            field::FieldType,
        },
        service::ServiceContext,
    };

    use super::*;

    fn setup() -> (Connection, CollectionDefinition, Arc<dyn PasswordProvider>) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                email TEXT UNIQUE,
                _password_hash TEXT,
                _locked INTEGER DEFAULT 0,
                _verified INTEGER DEFAULT 0,
                _session_version INTEGER DEFAULT 0,
                _reset_token TEXT,
                _reset_token_exp INTEGER,
                _verification_token TEXT,
                _verification_token_exp INTEGER,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("users");
        def.auth = Some(Auth {
            verify_email: true,
            ..Default::default()
        });
        def.fields = vec![
            FieldDefinition::builder("email", FieldType::Email)
                .unique(true)
                .build(),
        ];

        let provider: Arc<dyn PasswordProvider> = Arc::new(Argon2PasswordProvider);

        conn.execute(
            "INSERT INTO users (id, email, _verified) VALUES ('u1', 'test@example.com', 1)",
            [],
        )
        .unwrap();

        let hash = provider.hash_password("secret123").unwrap();
        conn.execute(
            "UPDATE users SET _password_hash = ?1 WHERE id = 'u1'",
            [hash.as_ref()],
        )
        .unwrap();

        (conn, def, provider)
    }

    #[test]
    fn authenticate_local_success() {
        let (conn, def, provider) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
        let result = authenticate_local(&ctx, "test@example.com", "secret123", &*provider, true);
        assert!(result.is_ok());
        let auth = result.unwrap();
        assert_eq!(auth.user.id, "u1");
        assert_eq!(auth.session_version, 0);
    }

    #[test]
    fn authenticate_local_wrong_password() {
        let (conn, def, provider) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
        let result = authenticate_local(&ctx, "test@example.com", "wrong", &*provider, true);
        assert!(matches!(result, Err(ServiceError::InvalidCredentials)));
    }

    #[test]
    fn authenticate_local_user_not_found() {
        let (conn, def, provider) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
        let result = authenticate_local(&ctx, "nobody@example.com", "secret123", &*provider, true);
        assert!(matches!(result, Err(ServiceError::InvalidCredentials)));
    }

    #[test]
    fn authenticate_local_locked() {
        let (conn, def, provider) = setup();
        conn.execute("UPDATE users SET _locked = 1 WHERE id = 'u1'", [])
            .unwrap();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
        let result = authenticate_local(&ctx, "test@example.com", "secret123", &*provider, true);
        assert!(matches!(result, Err(ServiceError::AccountLocked)));
    }

    #[test]
    fn authenticate_local_not_verified() {
        let (conn, def, provider) = setup();
        conn.execute("UPDATE users SET _verified = 0 WHERE id = 'u1'", [])
            .unwrap();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
        let result = authenticate_local(&ctx, "test@example.com", "secret123", &*provider, true);
        assert!(matches!(result, Err(ServiceError::EmailNotVerified)));
    }

    #[test]
    fn authenticate_local_not_verified_ignored_when_not_required() {
        let (conn, def, provider) = setup();
        conn.execute("UPDATE users SET _verified = 0 WHERE id = 'u1'", [])
            .unwrap();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
        let result = authenticate_local(&ctx, "test@example.com", "secret123", &*provider, false);
        assert!(result.is_ok());
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

    #[tokio::test]
    async fn lock_user_publishes_invalidation_when_transport_set() {
        use crate::core::event::{InProcessInvalidationBus, SharedInvalidationTransport};

        let (conn, def, _) = setup();
        let bus = Arc::new(InProcessInvalidationBus::new());
        let transport: SharedInvalidationTransport = bus;
        let mut rx = transport.subscribe();

        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .invalidation_transport(Some(transport))
            .build();

        lock_user(&ctx, "u1").unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("recv timed out")
            .expect("expected invalidation signal");
        assert_eq!(received, "u1");
    }

    #[test]
    fn lock_user_without_transport_is_noop() {
        let (conn, def, _) = setup();

        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        // Must succeed and not panic even with no transport attached.
        lock_user(&ctx, "u1").unwrap();

        let locked: i64 = conn
            .query_row("SELECT _locked FROM users WHERE id = 'u1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(locked, 1);
    }
}
