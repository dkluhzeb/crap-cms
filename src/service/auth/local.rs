//! Password-based authentication: the canonical `email + password →
//! AuthResult` flow shared between admin form POST and gRPC Login.
//!
//! Callers still own rate limiting, MFA, auth strategies, token
//! creation, and response formatting.

use crate::{
    core::{Document, auth::PasswordProvider},
    db::query,
    service::{ServiceContext, ServiceError},
};

/// Result of a successful local authentication.
pub struct AuthResult {
    pub user: Document,
    pub session_version: u64,
}

/// Authenticate a user by email and password.
///
/// Performs: `find_by_email` → `verify_password` → `check_locked`
/// → `check_verified` → `session_version`. Returns
/// `InvalidCredentials` if the user is not found or the password
/// is wrong.
///
/// # Errors
///
/// Returns `InvalidCredentials` when the email is unknown or the
/// password verification fails, `AccountLocked` when the account
/// is locked, `EmailNotVerified` when `require_verified` is set
/// and the user's email hasn't been verified, or a backend error
/// if the DB query fails.
pub fn authenticate_local(
    ctx: &ServiceContext,
    email: &str,
    password: &str,
    password_provider: &dyn PasswordProvider,
    require_verified: bool,
) -> Result<AuthResult, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;

    let Some(user) = query::find_by_email(conn, ctx.slug, def, email)? else {
        password_provider.dummy_verify();
        return Err(ServiceError::InvalidCredentials);
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

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::service::auth::test_support::setup;

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
}
