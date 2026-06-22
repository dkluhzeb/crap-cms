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
    Some(hook_runner.check_access(
        &AccessCheckInput {
            document: None,
            access: Some(access),
            user: Some(user_doc),
            id: None,
            data: None,
            locale: None,
            operation: "admin",
            collection: "admin",
            ui_locale: None,
        },
        &conn,
    ))
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
    let pool = state.pool.clone();
    let hook_runner = state.hook_runner.clone();
    let user_doc = user_doc.clone();

    let result = spawn_blocking(move || {
        check_admin_access_blocking(&pool, &hook_runner, &access, &user_doc)
    })
    .await;

    match result {
        // Allowed (or constrained — the admin gate has no row scope to filter)
        // is the ONLY outcome that lets the request through.
        Ok(Some(Ok(query::AccessResult::Allowed | query::AccessResult::Constrained(_)))) => None,
        Ok(Some(Ok(query::AccessResult::Denied))) => Some(admin_denied_response(state)),
        Ok(Some(Err(e))) => {
            error!("admin.access check failed: {}", e);
            Some(admin_denied_response(state))
        }
        // Pool exhaustion (`Ok(None)`) or a spawn-join failure (`Err`) must
        // fail CLOSED — never silently admit when the gate could not run.
        Ok(None) | Err(_) => {
            error!("admin.access gate could not run (pool exhausted or task join failed); denying");
            Some(admin_denied_response(state))
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
        .registry
        .get_collection(slug)
        .map(|d| &d.access)
        .or_else(|| state.registry.get_global(slug).map(|d| &d.access))?
        .admin
        .clone()?;
    let pool = state.pool.clone();
    let hook_runner = state.hook_runner.clone();
    let slug_owned = slug.to_string();
    let user_doc = user_doc.clone();

    let result = spawn_blocking(move || {
        let conn = pool.get().ok()?;
        Some(hook_runner.check_access(
            &AccessCheckInput {
                document: None,
                access: Some(&access),
                user: Some(&user_doc),
                id: None,
                data: None,
                locale: None,
                operation: "admin",
                collection: &slug_owned,
                ui_locale: None,
            },
            &conn,
        ))
    })
    .await;

    match result {
        Ok(Some(Ok(query::AccessResult::Allowed | query::AccessResult::Constrained(_)))) => None,
        Ok(Some(Ok(query::AccessResult::Denied))) => Some(admin_denied_response(state)),
        Ok(Some(Err(e))) => {
            error!("collection access.admin check failed for '{slug}': {e}");
            Some(admin_denied_response(state))
        }
        Ok(None) | Err(_) => {
            error!("collection access.admin gate could not run for '{slug}'; denying");
            Some(admin_denied_response(state))
        }
    }
}
