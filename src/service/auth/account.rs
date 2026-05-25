//! Account state operations: lock / unlock, verified flag,
//! session-version bump, user existence. These are thin
//! `ServiceContext` wrappers around `query::*` writes; they exist
//! to keep call-site code path-agnostic between admin and gRPC.

use crate::{
    db::query,
    service::{ServiceContext, ServiceError},
};

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
/// # Errors
///
/// Returns a backend error if the DB connection or update fails.
pub fn bump_session_version(ctx: &ServiceContext, id: &str) -> Result<u64, ServiceError> {
    let conn = ctx.resolve_conn()?;
    Ok(query::bump_session_version(conn.as_ref(), ctx.slug, id)?)
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
/// # Errors
///
/// Returns a backend error if the DB connection or update fails.
pub fn mark_unverified(ctx: &ServiceContext, id: &str) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    query::mark_unverified(conn.as_ref(), ctx.slug, id)?;
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
