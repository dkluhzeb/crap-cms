//! Account state operations: lock / unlock, verified flag,
//! session-version bump, user existence. These are thin
//! `ServiceContext` wrappers around `query::*` writes; they exist
//! to keep call-site code path-agnostic between admin and gRPC.

use crate::{
    db::{AccessResult, query},
    hooks::AccessCheckInput,
    service::{ServiceContext, ServiceError, helpers::enforce_access_constraints},
};

/// An administrative account-state action on another user's auth document.
///
/// Each variant is authorized by an access function before the underlying write
/// runs (see [`perform_account_action`]): the lock-state pair by
/// `access.unlock` (falling back to `update`), the verification pair by
/// `access.update` — mirroring how the admin UI gates a lock toggle behind the
/// document `update` access.
#[derive(Debug, Clone, Copy)]
pub enum AccountAction {
    Lock,
    Unlock,
    Verify,
    Unverify,
}

impl AccountAction {
    /// The `ctx.operation` label the access function sees.
    fn operation(self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::Unlock => "unlock",
            Self::Verify => "verify",
            Self::Unverify => "unverify",
        }
    }
}

/// Authorize and perform an administrative account-state action on the user
/// `id` in `ctx`'s (auth) collection.
///
/// The caller (`ctx.user`) is authorized against the target document before the
/// write: the lock-state actions resolve `access.unlock` (→ `update`), the
/// verification actions resolve `access.update`. A `Denied` result rejects; a
/// `Constrained` (row-filter) result must match the target user (e.g. an
/// org-scoped moderator) — enforced via [`enforce_access_constraints`]. This is
/// the single gate both the gRPC account RPCs and (future) other surfaces share,
/// so an authenticated-but-unauthorized caller can never mutate another
/// account's state.
///
/// `ctx` must be a collection context carrying the collection definition, the
/// caller (`user`), a hook `runner`, and a `conn`.
///
/// # Errors
///
/// Returns [`ServiceError::AccessDenied`] when the caller may not perform the
/// action on the target, or a hook/backend error.
pub fn perform_account_action(
    ctx: &ServiceContext,
    id: &str,
    action: AccountAction,
) -> Result<(), ServiceError> {
    let def = ctx.collection_def()?;
    let access_ref = match action {
        AccountAction::Lock | AccountAction::Unlock => def.access.resolve_unlock(),
        AccountAction::Verify | AccountAction::Unverify => def.access.update.as_ref(),
    };

    let conn = ctx.resolve_conn()?;
    let runner = ctx.runner()?;

    let access = runner.check_access(
        &AccessCheckInput {
            document: None,
            access: access_ref,
            user: ctx.user,
            id: Some(id),
            data: None,
            locale: None,
            operation: action.operation(),
            collection: ctx.slug,
            ui_locale: None,
        },
        conn.as_ref(),
    )?;

    if matches!(access, AccessResult::Denied) {
        return Err(ServiceError::AccessDenied(format!(
            "{} access denied",
            action.operation()
        )));
    }

    // A row-filter (Constrained) rule scopes which users the caller may act on
    // (e.g. `{ org = ctx.user.org }`) — enforce it against the target.
    enforce_access_constraints(ctx, id, &access, action.operation(), false)?;

    match action {
        AccountAction::Lock => lock_user(ctx, id),
        AccountAction::Unlock => unlock_user(ctx, id),
        AccountAction::Verify => mark_verified(ctx, id),
        AccountAction::Unverify => mark_unverified(ctx, id),
    }
}

/// Lock a user account, preventing login.
///
/// Bumps `_session_version` so every JWT issued before the lock
/// is rejected on its next use (the evaluator's session-version
/// check trips). Also publishes a user-invalidation signal so
/// any active live-update streams owned by this user are torn
/// down — both checks are needed because the lock test and the
/// session-version test fire at different points (request-arrival
/// vs. stream-evaluation).
///
/// # Errors
///
/// Returns a backend error if the DB connection or update fails.
pub fn lock_user(ctx: &ServiceContext, id: &str) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    query::lock_user(conn.as_ref(), ctx.slug, id)?;
    let _ = query::bump_session_version(conn.as_ref(), ctx.slug, id)?;
    ctx.publish_user_invalidation(id);
    Ok(())
}

/// Bump the user's `_session_version`, invalidating every JWT
/// issued before this call across all of that user's surfaces.
/// Used by logout to close out a session server-side (cookie
/// clearing alone leaves the token usable by a thief who has
/// captured it).
///
/// Also publishes a user-invalidation signal so the user's open
/// live-update streams are torn down — exactly like [`lock_user`] /
/// [`mark_unverified`] / a password reset. The version bump alone
/// only rejects *new* requests; an already-connected SSE/subscribe
/// stream never re-reads `_session_version`, so it would keep
/// delivering on the revoked session until the client reconnects.
/// (No-op when the context carries no invalidation transport.)
///
/// # Errors
///
/// Returns a backend error if the DB connection or update fails.
pub fn bump_session_version(ctx: &ServiceContext, id: &str) -> Result<u64, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let version = query::bump_session_version(conn.as_ref(), ctx.slug, id)?;
    ctx.publish_user_invalidation(id);
    Ok(version)
}

/// Unlock a user account.
///
/// # Errors
///
/// Returns a backend error if the DB connection or update fails.
pub fn unlock_user(ctx: &ServiceContext, id: &str) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    query::unlock_user(conn.as_ref(), ctx.slug, id)?;
    Ok(())
}

/// Mark a user's email as verified.
///
/// # Errors
///
/// Returns a backend error if the DB connection or update fails.
pub fn mark_verified(ctx: &ServiceContext, id: &str) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    query::mark_verified(conn.as_ref(), ctx.slug, id)?;
    Ok(())
}

/// Mark a user's email as unverified.
///
/// Bumps the session version and publishes a user invalidation, exactly like
/// [`lock_user`] / a password reset: un-verifying is a privilege-revoking
/// action, and the per-request resolver only re-evaluates lock + session
/// version (not verification). Without the bump an unverified user keeps a
/// usable session — and could refresh it indefinitely — until expiry.
///
/// # Errors
///
/// Returns a backend error if the DB connection or update fails.
pub fn mark_unverified(ctx: &ServiceContext, id: &str) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    query::mark_unverified(conn.as_ref(), ctx.slug, id)?;
    let _ = query::bump_session_version(conn.as_ref(), ctx.slug, id)?;
    ctx.publish_user_invalidation(id);
    Ok(())
}

/// Check whether a user account is locked.
///
/// # Errors
///
/// Returns a backend error if the DB connection or query fails.
pub fn is_locked(ctx: &ServiceContext, id: &str) -> Result<bool, ServiceError> {
    let conn = ctx.resolve_conn()?;
    Ok(query::is_locked(conn.as_ref(), ctx.slug, id)?)
}

/// Check whether a user's email is verified.
///
/// # Errors
///
/// Returns a backend error if the DB connection or query fails.
pub fn is_verified(ctx: &ServiceContext, id: &str) -> Result<bool, ServiceError> {
    let conn = ctx.resolve_conn()?;
    Ok(query::is_verified(conn.as_ref(), ctx.slug, id)?)
}

/// Get the current session version for a user (for JWT invalidation).
///
/// # Errors
///
/// Returns a backend error if the DB connection or query fails.
pub fn get_session_version(ctx: &ServiceContext, id: &str) -> Result<u64, ServiceError> {
    let conn = ctx.resolve_conn()?;
    Ok(query::get_session_version(conn.as_ref(), ctx.slug, id)?)
}

/// Check whether a user document exists.
///
/// # Errors
///
/// Returns a backend error if the DB connection or query fails.
pub fn user_exists(ctx: &ServiceContext, id: &str) -> Result<bool, ServiceError> {
    let conn = ctx.resolve_conn()?;
    Ok(query::user_exists(conn.as_ref(), ctx.slug, id)?)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::core::event::{InProcessInvalidationBus, SharedInvalidationTransport};
    use crate::service::auth::test_support::setup;
    use std::sync::Arc;

    #[tokio::test]
    async fn lock_user_publishes_invalidation_when_transport_set() {
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

    /// Regression: `lock_user` must bump `_session_version`. If
    /// it doesn't, JWTs that were issued before the lock keep
    /// working until exp — defeating the lock's intent for
    /// long-lived bearer tokens. The evaluator's locked check
    /// (`is_locked` on the bearer/cookie/strategy paths) is one
    /// layer of defense; the version bump is the second.
    #[test]
    fn lock_user_bumps_session_version() {
        let (conn, def, _) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        let before = get_session_version(&ctx, "u1").unwrap();

        lock_user(&ctx, "u1").unwrap();

        let after = get_session_version(&ctx, "u1").unwrap();
        assert!(
            after > before,
            "lock_user must increment _session_version (was {before}, now {after})"
        );
    }

    /// Regression: `mark_unverified` must bump `_session_version` (like
    /// `lock_user`). Un-verifying revokes access to a verify-gated collection,
    /// but the per-request resolver re-checks lock + version, NOT verification —
    /// so without the bump an unverified user keeps a usable session (and could
    /// refresh it) until expiry.
    #[test]
    fn mark_unverified_bumps_session_version() {
        let (conn, def, _) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        let before = get_session_version(&ctx, "u1").unwrap();

        mark_unverified(&ctx, "u1").unwrap();

        let after = get_session_version(&ctx, "u1").unwrap();
        assert!(
            after > before,
            "mark_unverified must increment _session_version (was {before}, now {after})"
        );
    }

    /// Regression: `bump_session_version` (the logout primitive) must publish a
    /// user-invalidation signal so the user's open live-update streams are torn
    /// down — like `lock_user` / a password reset. The version bump alone only
    /// rejects new requests; an already-connected SSE/subscribe stream never
    /// re-reads `_session_version`, so without the publish a captured token with
    /// an open stream survives the legitimate user's logout.
    #[tokio::test]
    async fn bump_session_version_publishes_invalidation_when_transport_set() {
        let (conn, def, _) = setup();
        let bus = Arc::new(InProcessInvalidationBus::new());
        let transport: SharedInvalidationTransport = bus;
        let mut rx = transport.subscribe();

        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .invalidation_transport(Some(transport))
            .build();

        bump_session_version(&ctx, "u1").unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("recv timed out")
            .expect("expected invalidation signal");
        assert_eq!(received, "u1");
    }

    /// `bump_session_version` is the standalone primitive backing
    /// logout / lock / password reset. Monotonic increment, returns
    /// the new value.
    #[test]
    fn bump_session_version_increments_and_returns_new_value() {
        let (conn, def, _) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        let v0 = get_session_version(&ctx, "u1").unwrap();
        let v1 = bump_session_version(&ctx, "u1").unwrap();
        assert_eq!(v1, v0 + 1);
        let v2 = bump_session_version(&ctx, "u1").unwrap();
        assert_eq!(v2, v1 + 1);
    }
}
