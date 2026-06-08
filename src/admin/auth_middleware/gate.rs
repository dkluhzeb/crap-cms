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
use crate::core::{AuthUser, Document};
use crate::db::{DbPool, query};
use crate::hooks::{AccessCheckInput, HookRunner};

/// Blocking body for the admin-access gate's `spawn_blocking`
/// call. Pulls a connection from the pool and runs the configured
/// `admin.access` Lua hook. Returns `None` only when the pool is
/// exhausted; pool failure is treated as a transient deny-by-
/// default upstream.
#[cfg(not(tarpaulin_include))]
fn check_admin_access_blocking(
    pool: &DbPool,
    hook_runner: &HookRunner,
    access_ref: &str,
    user_doc: &Document,
) -> Option<Result<query::AccessResult, anyhow::Error>> {
    let conn = pool.get().ok()?;
    Some(hook_runner.check_access(
        &AccessCheckInput {
            access_ref: Some(access_ref),
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
    let access_ref = state.config.admin.access.as_deref()?;
    let pool = state.pool.clone();
    let hook_runner = state.hook_runner.clone();
    let user_doc = user_doc.clone();
    let access_ref = access_ref.to_string();

    let result = spawn_blocking(move || {
        check_admin_access_blocking(&pool, &hook_runner, &access_ref, &user_doc)
    })
    .await;

    match result {
        Ok(Some(Ok(query::AccessResult::Denied))) => Some(admin_denied_response(state)),
        Ok(Some(Err(e))) => {
            error!("admin.access check failed: {}", e);
            Some(admin_denied_response(state))
        }
        _ => None,
    }
}
