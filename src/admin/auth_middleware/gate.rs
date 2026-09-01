//! `admin.access` gate — runs the operator-configured Lua hook
//! against an authenticated user document and returns a 403 page
//! when access is denied. Called by both the middleware
//! (post-resolution) and by `login_action` (before issuing a
//! session cookie, so the login form itself surfaces "you can't
//! reach admin" instead of dropping the user behind the gate with
//! a useless session).

use axum::response::Response;
use tokio::task::spawn_blocking;
use tracing::error;

use crate::admin::{AdminState, auth_middleware::pages::admin_denied_response};
use crate::core::{AuthUser, Document, HookRef};
use crate::db::{DbPool, query};
use crate::hooks::{AccessCheckInput, HookRunner};

/// Blocking body for the admin-access gate's `spawn_blocking`
/// call. Pulls a connection from the pool and runs the configured
/// `admin.access` Lua hook. Returns `None` when the pool is
/// exhausted — the caller treats that (and a join failure) as a
/// denial, so the gate fails **closed**.
#[cfg(not(tarpaulin_include))]
fn check_admin_access_blocking(
    pool: &DbPool,
    hook_runner: &HookRunner,
    access: &HookRef,
    user_doc: &Document,
) -> Option<Result<query::AccessResult, anyhow::Error>> {
    let conn = pool.get().ok()?;
    Some(
        hook_runner.check_access(
            &AccessCheckInput::builder("admin", "admin")
                .access(Some(access))
                .user(Some(user_doc))
                .build(),
            &conn,
        ),
    )
}

/// Gate 2: Check `admin.access` Lua function. Returns a 403
/// response if the user is denied, or `None` if access is allowed
/// (or no access function is configured).
#[cfg(not(tarpaulin_include))]
pub(super) async fn check_admin_gate(state: &AdminState, auth_user: &AuthUser) -> Option<Response> {
    check_admin_gate_for_doc(state, &auth_user.user_doc).await
}

/// Check `admin.access` against a user document. Used by both the
/// auth middleware and the login handler to enforce the gate
/// before issuing a session.
#[cfg(not(tarpaulin_include))]
pub(crate) async fn check_admin_gate_for_doc(
    state: &AdminState,
    user_doc: &Document,
) -> Option<Response> {
    let access = state.config.admin.access.clone()?;
    let pool = state.infra.pool.clone();
    let hook_runner = state.infra.hook_runner.clone();
    let user_doc = user_doc.clone();

    let result = spawn_blocking(move || {
        check_admin_access_blocking(&pool, &hook_runner, &access, &user_doc)
    })
    .await;

    // A spawn-join failure (`Err`) flattens to `None` — fails CLOSED below.
    if gate_passes(result.ok().flatten(), "admin.access") {
        return None;
    }

    Some(admin_denied_response(state))
}

/// Map an `admin.access` / `access.admin` outcome to pass (`true`) or deny.
///
/// The gate is **boolean**: `Allowed` is the ONLY pass. A `Constrained`
/// filter table is a rule-author mistake — there is no row scope to filter at
/// the admin gate — and is treated as a denial (logged as an error), matching
/// `access.mcp`, instead of silently admitting. A hook error, pool exhaustion
/// (`None`) or a task-join failure also deny: never admit when the gate could
/// not run.
fn gate_passes(outcome: Option<Result<query::AccessResult, anyhow::Error>>, what: &str) -> bool {
    match outcome {
        Some(Ok(query::AccessResult::Allowed)) => true,
        Some(Ok(query::AccessResult::Denied)) => false,
        Some(Ok(query::AccessResult::Constrained(_))) => {
            error!(
                "{what} returned a filter table — the admin gate is boolean (return true/false); denying"
            );
            false
        }
        Some(Err(e)) => {
            error!("{what} check failed: {e}");
            false
        }
        None => {
            error!("{what} gate could not run (pool exhausted or task join failed); denying");
            false
        }
    }
}

/// Per-collection / per-global `access.admin` gate. Runs the `admin` rule of the
/// collection or global named `slug` against the authenticated user for any
/// admin route scoped to it, returning a 403 page on denial. **Permissive
/// default:** a slug with no `access.admin` (or one that isn't a collection or
/// global) is always allowed (`None`), so this only ever *further* restricts
/// admin-UI access beyond `read`. Fails CLOSED if the rule errors or the gate
/// can't run.
#[cfg(not(tarpaulin_include))]
pub(crate) async fn check_collection_admin_gate(
    state: &AdminState,
    slug: &str,
    user_doc: &Document,
) -> Option<Response> {
    let access = state
        .infra
        .registry
        .get_collection(slug)
        .map(|d| &d.access)
        .or_else(|| state.infra.registry.get_global(slug).map(|d| &d.access))?
        .admin
        .clone()?;
    let pool = state.infra.pool.clone();
    let hook_runner = state.infra.hook_runner.clone();
    let slug_owned = slug.to_string();
    let user_doc = user_doc.clone();

    let result = spawn_blocking(move || {
        check_collection_admin_access_blocking(&pool, &hook_runner, &access, &slug_owned, &user_doc)
    })
    .await;

    if gate_passes(result.ok().flatten(), &format!("access.admin for '{slug}'")) {
        return None;
    }

    Some(admin_denied_response(state))
}

/// `spawn_blocking` body for [`check_collection_admin_gate`]: run the
/// per-collection / per-global `access.admin` Lua hook against `user_doc`. Mirror
/// of [`check_admin_access_blocking`] but keyed to `slug` (the operation runs
/// with `collection = slug`). Returns `None` on pool exhaustion so the caller
/// fails **closed**.
#[cfg(not(tarpaulin_include))]
fn check_collection_admin_access_blocking(
    pool: &DbPool,
    hook_runner: &HookRunner,
    access: &HookRef,
    slug: &str,
    user_doc: &Document,
) -> Option<Result<query::AccessResult, anyhow::Error>> {
    let conn = pool.get().ok()?;
    Some(
        hook_runner.check_access(
            &AccessCheckInput::builder("admin", slug)
                .access(Some(access))
                .user(Some(user_doc))
                .build(),
            &conn,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::query::AccessResult;

    /// Regression: a `Constrained` filter table used to PASS the admin gate
    /// (`Allowed | Constrained(_)`), contradicting the documented "boolean
    /// rule" and the fail-closed `access.mcp` twin.
    #[test]
    fn admin_gate_is_boolean_and_fails_closed() {
        assert!(gate_passes(Some(Ok(AccessResult::Allowed)), "t"));
        assert!(!gate_passes(Some(Ok(AccessResult::Denied)), "t"));
        assert!(!gate_passes(
            Some(Ok(AccessResult::Constrained(vec![]))),
            "t"
        ));
        assert!(!gate_passes(Some(Err(anyhow::anyhow!("boom"))), "t"));
        assert!(!gate_passes(None, "t"));
    }
}
