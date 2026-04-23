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
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::auth::{
            client_ip, create_session_token, extract_user_email, find_auth_collection,
            headers_to_map, session_redirect,
        },
    },
    core::Document,
    service::{self, ServiceContext},
};

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
    let hook_ref = format!("auth_callback.{}", name);
    let pool = state.pool.clone();
    let hook_runner = state.hook_runner.clone();
    let collection = collection.to_string();

    let mut ctx = headers_to_map(headers);

    for (k, v) in params {
        ctx.insert(format!("_query_{}", k), v.clone());
    }

    let result = task::spawn_blocking(move || {
        let conn = pool.get()?;

        hook_runner
            .run_auth_strategy(&hook_ref, &collection, &ctx, &conn)
            .map_err(|e| anyhow!("Auth callback hook error: {:#}", e))
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

/// Fetch the session version for a user, returning `None` on DB errors.
fn fetch_session_version(state: &AdminState, slug: &str, user_id: &str) -> Option<u64> {
    let conn = state.pool.get().ok()?;
    let ctx = ServiceContext::slug_only(slug).conn(&conn).build();

    Some(service::auth::get_session_version(&ctx, user_id).unwrap_or(0))
}

/// GET/POST `/admin/auth/callback/{name}` — dispatch to Lua auth callback hook.
///
/// The hook function `hooks.auth_callback.{name}` receives:
/// - `query` — URL query parameters as key-value table
/// - `headers` — HTTP request headers as key-value table
/// - `method` — HTTP method string ("GET" or "POST")
///
/// Returns a user document table (with `id` field) to create a session,
/// or `nil`/`false` to redirect to login.
pub async fn auth_callback(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let ip = client_ip(&headers, &addr, &state.config.server);

    if state.ip_login_limiter.is_blocked(&ip) {
        return Redirect::to("/admin/login").into_response();
    }

    let collection = match find_auth_collection(&state.registry) {
        Some(c) => c,
        None => return Redirect::to("/admin/login").into_response(),
    };

    let user = match run_auth_callback_hook(&state, &name, &headers, &params, &collection).await {
        Ok(Some(doc)) => doc,
        Ok(None) => {
            state.ip_login_limiter.record_failure(&ip);
            return Redirect::to("/admin/login").into_response();
        }
        Err(()) => return Redirect::to("/admin/login").into_response(),
    };

    let session_version = match fetch_session_version(&state, &collection, &user.id) {
        Some(v) => v,
        None => return Redirect::to("/admin/login").into_response(),
    };

    let email = extract_user_email(&user);

    let session = match create_session_token(
        &state,
        user.id.to_string(),
        &collection,
        email,
        session_version,
    ) {
        Ok(s) => s,
        Err(e) => {
            error!("Auth callback: {}", e);
            return Redirect::to("/admin/login").into_response();
        }
    };

    state.ip_login_limiter.clear(&ip);

    session_redirect(&state, &session)
}
