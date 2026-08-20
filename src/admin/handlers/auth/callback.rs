//! Auth callback handler — dispatches `/admin/auth/callback/{name}` to Lua hooks.
//!
//! Enables OAuth2/OIDC and external auth providers implemented entirely in Lua.
//! The hook receives query parameters, headers, and method; returns a user
//! document to create a session, or nil to redirect to login with an error.

use std::{collections::HashMap, net::SocketAddr};

use anyhow::anyhow;
use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use tokio::task;
use tracing::{error, warn};

use crate::{
    admin::{
        AdminState,
        auth_middleware::check_admin_gate_for_doc,
        handlers::{
            auth::{
                client_ip, create_session_token, extract_user_email, headers_to_map,
                session_redirect, sole_auth_collection,
            },
            shared::paths,
        },
    },
    core::{Document, HookRef, collection::Auth},
    db::{DbPool, query},
    hooks::{HookRunner, lifecycle::AuthStrategyInput},
    service::{self, ServiceContext},
};

/// Pull a connection from the pool and execute the configured Lua auth
/// strategy hook for an external auth flow (OAuth callback etc.).
fn run_auth_strategy_blocking(
    pool: &DbPool,
    hook_runner: &HookRunner,
    hook_ref: &str,
    collection: &str,
    ctx: &HashMap<String, String>,
) -> anyhow::Result<Option<Document>> {
    let conn = pool.get()?;
    let input = AuthStrategyInput {
        collection,
        headers: ctx,
        email: None,
        password: None,
        remote_addr: None,
    };
    // OAuth-callback hook refs are synthesized (`auth_callback.<name>`), not
    // config-declared, so they carry no per-config options.
    let strategy = HookRef::new(hook_ref);
    hook_runner
        .run_auth_strategy(&strategy, &input, &conn)
        .map_err(|e| anyhow!("Auth callback hook error: {e:#}"))
}

/// Run the Lua auth callback hook in a blocking task.
///
/// Returns `Ok(Some(doc))` on success, `Ok(None)` if the hook returned nil or
/// had an application error (caller should rate-limit), `Err` on system failure
/// (task panic — caller should NOT rate-limit).
async fn run_auth_callback_hook(
    state: &AdminState,
    name: &str,
    headers: &HeaderMap,
    params: &HashMap<String, String>,
    collection: &str,
) -> Result<Option<Document>, ()> {
    let hook_ref = format!("auth_callback.{name}");
    let pool = state.pool.clone();
    let hook_runner = state.hook_runner.clone();
    let collection = collection.to_string();

    let mut ctx = headers_to_map(headers);

    for (k, v) in params {
        ctx.insert(format!("_query_{k}"), v.clone());
    }

    let result = task::spawn_blocking(move || {
        run_auth_strategy_blocking(&pool, &hook_runner, &hook_ref, &collection, &ctx)
    })
    .await;

    match result {
        Ok(Ok(doc)) => Ok(doc),
        Ok(Err(e)) => {
            error!("Auth callback error: {:#}", e);
            Ok(None)
        }
        Err(e) => {
            error!("Auth callback task error: {}", e);
            Err(())
        }
    }
}

/// Validate the user returned by an auth-callback hook the same way the login
/// path does, and return the user's current session version.
///
/// `slug` is the collection the callback runs under — the **only** collection a
/// callback session may bind to. The session is bound here, NOT to whatever
/// collection happens to contain `user_id`: binding by id across collections
/// would let a hook-returned (or OAuth-provider-influenced) id mint a session
/// for a *different*, possibly higher-privilege, auth collection. So the user
/// must exist in `slug`; a returned id that lives only in another auth
/// collection is refused. (Multi-auth-collection OAuth — binding to a non-first
/// collection — needs a collection-scoped callback route; tracked follow-up.)
///
/// Rejects accounts not in `slug`, locked accounts, and — when the collection
/// requires email verification — unverified accounts, returning `None` so the
/// caller redirects to login without minting a session. Mirrors the per-strategy
/// login guard (`is_locked` + `requires_verify_email && is_verified`); without
/// it an external callback would bypass email verification, since the
/// per-request resolver re-checks lock and session version but **not**
/// verification.
fn validate_callback_user(state: &AdminState, slug: &str, user_id: &str) -> Option<u64> {
    let requires_verify = state
        .registry
        .get_collection(slug)
        .and_then(|def| def.auth.as_ref())
        .is_some_and(Auth::requires_verify_email);

    let conn = state.pool.get().ok()?;
    let ctx = ServiceContext::slug_only(slug).conn(&conn).build();

    // Bind only to `slug`: the user must exist in this collection. This is the
    // guard that prevents a cross-collection (privilege-escalating) binding.
    if !query::auth::user_exists(&conn, slug, user_id).ok()? {
        return None;
    }

    if service::auth::is_locked(&ctx, user_id).ok()? {
        return None;
    }

    if requires_verify && !service::auth::is_verified(&ctx, user_id).ok()? {
        return None;
    }

    // Fail CLOSED like every check above (`is_locked`/`is_verified`/`user_exists`
    // all propagate DB errors as `None`): a transient DB error must NOT mint a
    // session. The old `.unwrap_or(0)` mapped a query failure to session_version
    // 0 — for the common user whose real version is 0 that minted a fully valid
    // session on a DB error, out of step with the fail-closed session-version
    // read in the token-validation path (evaluator.rs).
    service::auth::get_session_version(&ctx, user_id).ok()
}

/// Run the rate-limit → hook → validate → mint-session flow for a callback that
/// has already resolved its target auth `collection`. Shared by the legacy
/// un-scoped handler ([`auth_callback`]) and the collection-scoped handler
/// ([`super::callback_scoped::auth_callback_scoped`]).
///
/// The session binds ONLY to `collection`: the hook-returned user must exist in
/// it (enforced by [`validate_callback_user`]), so a returned id that lives only
/// in another auth collection is refused — no cross-collection (privilege-
/// escalating) binding regardless of which route reached here.
pub(super) async fn complete_auth_callback(
    state: &AdminState,
    addr: SocketAddr,
    collection: &str,
    name: &str,
    params: &HashMap<String, String>,
    headers: &HeaderMap,
) -> Response {
    let ip = client_ip(headers, &addr, &state.config.server);

    // Atomically record this callback attempt against the IP limiter and bail
    // if over threshold — one operation, closing the burst race the is_blocked
    // + later record_failure split left open. A successful login clears it below.
    if state.ip_login_limiter.check_and_block(&ip) {
        return Redirect::to(paths::LOGIN).into_response();
    }

    let user = match run_auth_callback_hook(state, name, headers, params, collection).await {
        Ok(Some(doc)) => doc,
        Ok(None) => {
            // Attempt already recorded up front by check_and_block.
            return Redirect::to(paths::LOGIN).into_response();
        }
        Err(()) => return Redirect::to(paths::LOGIN).into_response(),
    };

    // The returned user must exist in `collection`; lock + verify-email enforced.
    let Some(session_version) = validate_callback_user(state, collection, &user.id) else {
        return Redirect::to(paths::LOGIN).into_response();
    };

    // Authentication succeeded — clear the IP limiter (mirror the password login
    // path, which clears before the gate check so a denied user doesn't count
    // toward the threshold).
    state.ip_login_limiter.clear(&ip);

    // Enforce the `admin.access` gate BEFORE minting a session — parity with the
    // password login path. Without this a denied user still gets a valid session
    // cookie and is only stopped by the middleware on first page load (a useless
    // cookie + 403 instead of a clean denial).
    if let Some(response) = check_admin_gate_for_doc(state, &user).await {
        return response;
    }

    let email = extract_user_email(&user);

    let session = match create_session_token(
        state,
        user.id.to_string(),
        collection,
        email,
        session_version,
        Utc::now().timestamp().max(0).cast_unsigned(),
    ) {
        Ok(s) => s,
        Err(e) => {
            error!("Auth callback: {}", e);
            return Redirect::to(paths::LOGIN).into_response();
        }
    };

    session_redirect(state, &session)
}

/// GET/POST `/admin/auth/callback/{name}` — un-scoped auth callback dispatch.
///
/// The hook function `hooks.auth_callback.{name}` receives:
/// - `query` — URL query parameters as key-value table
/// - `headers` — HTTP request headers as key-value table
/// - `method` — HTTP method string ("GET" or "POST")
///
/// Returns a user document table (with `id` field) to create a session,
/// or `nil`/`false` to redirect to login.
///
/// This route binds to the SINGLE auth collection when there is exactly one.
/// With 2+ auth collections the target is ambiguous, so it fails closed — use
/// the collection-scoped route `/admin/auth/callback/{collection}/{name}`.
pub async fn auth_callback(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(collection) = sole_auth_collection(&state.registry) else {
        warn!(
            "un-scoped auth callback {name:?} has no unambiguous auth collection \
             (need exactly one); use /admin/auth/callback/{{collection}}/{name} \
             for multi-auth-collection setups"
        );
        return Redirect::to(paths::LOGIN).into_response();
    };

    complete_auth_callback(&state, addr, &collection, &name, &params, &headers).await
}
