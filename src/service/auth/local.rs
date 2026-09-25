//! Password-based authentication: the canonical `email + password →
//! AuthResult` flow shared between admin form POST and gRPC Login.
//!
//! Callers still own rate limiting, MFA, auth strategies, token
//! creation, and response formatting.

use crate::{
    core::{Document, HashedPassword, auth::PasswordProvider},
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
/// The lookups run in two short connection scopes around the password
/// verification, never across it: given a pool-backed `ctx`, no connection
/// is held while Argon2 runs, so a burst of logins cannot starve every other
/// request of connections for the length of a hash.
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
    let Some((user, hash)) = find_credentials(ctx, email)? else {
        password_provider.dummy_verify();
        return Err(ServiceError::InvalidCredentials);
    };

    let verified = match hash {
        Some(hash) => password_provider.verify_password(password, hash.as_ref())?,
        None => false,
    };

    if !verified {
        return Err(ServiceError::InvalidCredentials);
    }

    account_standing(ctx, user, require_verified)
}

/// The account `email` names and its stored password hash, read on a
/// connection released before the caller verifies the password.
///
/// A soft-deleted (trashed) account is disabled: it is excluded so a trashed
/// user cannot authenticate, consistent with the evaluator's `find_by_id`
/// (which rejects an existing session for a trashed user as `UserMissing`).
fn find_credentials(
    ctx: &ServiceContext,
    email: &str,
) -> Result<Option<(Document, Option<HashedPassword>)>, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;

    let locale_ctx = ctx.default_locale_ctx();
    let Some(user) = query::find_by_email(conn, ctx.slug, def, email, false, locale_ctx.as_ref())?
    else {
        return Ok(None);
    };

    let hash = query::get_password_hash(conn, ctx.slug, &user.id)?;

    Ok(Some((user, hash)))
}

/// Settle a password-verified account: refuse it when locked (or unverified
/// where verification is required), otherwise read its session version.
fn account_standing(
    ctx: &ServiceContext,
    user: Document,
    require_verified: bool,
) -> Result<AuthResult, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();

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
    use anyhow::Result as AnyResult;

    use super::*;
    use crate::{
        core::auth::Argon2PasswordProvider,
        db::DbPool,
        service::auth::test_support::{setup, single_connection_pool},
    };

    /// Verifies through Argon2, but first checks out a connection from the
    /// pool the login runs against — which fails if the login still holds
    /// the pool's only connection.
    struct ProbingProvider {
        pool: DbPool,
    }

    impl PasswordProvider for ProbingProvider {
        fn hash_password(&self, password: &str) -> AnyResult<HashedPassword> {
            Argon2PasswordProvider.hash_password(password)
        }

        fn verify_password(&self, password: &str, hash: &str) -> AnyResult<bool> {
            self.pool.get()?;

            Argon2PasswordProvider.verify_password(password, hash)
        }

        fn dummy_verify(&self) {
            Argon2PasswordProvider.dummy_verify();
        }

        fn kind(&self) -> &'static str {
            "probing"
        }
    }

    /// Regression: the login held its database connection across the
    /// password hash, so concurrent logins pinned connections for the length
    /// of an Argon2 run each. The hash now runs with none held.
    #[test]
    fn no_connection_is_held_while_the_password_is_verified() {
        let (pool, def) = single_connection_pool("secret123");
        let provider = ProbingProvider { pool: pool.clone() };
        let ctx = ServiceContext::collection("users", &def)
            .pool(&pool)
            .build();

        let result = authenticate_local(&ctx, "test@example.com", "secret123", &provider, true);

        assert!(result.is_ok(), "{:?}", result.err());
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
}
