//! Auth callback handler — dispatches `/admin/auth/callback/{name}` to Lua hooks.
//!
//! Enables OAuth2/OIDC and external auth providers implemented entirely in Lua.
//! The hook receives query parameters, headers, and method; returns a user
//! document to create a session, or nil to redirect to login with an error.

use std::{collections::HashMap, net::SocketAddr};

use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};

use super::{helpers::client_ip, session::session_cookies};
use crate::{
    admin::AdminState,
    core::{Slug, auth::ClaimsBuilder},
    db::query,
};

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
    let ip = client_ip(&headers, &addr, state.config.server.trust_proxy);

    // Rate limit callback attempts by IP
    if state.ip_login_limiter.is_blocked(&ip) {
        return Redirect::to("/admin/login").into_response();
    }

    let header_map: HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_string())))
        .collect();

    let hook_ref = format!("auth_callback.{}", name);
    let pool = state.pool.clone();
    let hook_runner = state.hook_runner.clone();
    let registry = state.registry.clone();

    // Find the first auth collection for callback context
    let auth_collection = registry
        .collections
        .iter()
        .find(|(_, d)| d.is_auth_collection())
        .map(|(slug, _)| slug.to_string());

    let collection = match auth_collection {
        Some(c) => c,
        None => return Redirect::to("/admin/login").into_response(),
    };

    let collection_for_hook = collection.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;

        // Merge query params and headers into context
        let mut ctx = header_map;
        for (k, v) in &params {
            ctx.insert(format!("_query_{}", k), v.clone());
        }

        hook_runner
            .run_auth_strategy(&hook_ref, &collection_for_hook, &ctx, &conn)
            .map_err(|e| anyhow::anyhow!("Auth callback hook error: {:#}", e))
    })
    .await;

    let user = match result {
        Ok(Ok(Some(doc))) => doc,
        Ok(Ok(None)) => {
            state.ip_login_limiter.record_failure(&ip);
            return Redirect::to("/admin/login").into_response();
        }
        Ok(Err(e)) => {
            tracing::error!("Auth callback error: {:#}", e);
            state.ip_login_limiter.record_failure(&ip);
            return Redirect::to("/admin/login").into_response();
        }
        Err(e) => {
            tracing::error!("Auth callback task error: {}", e);
            return Redirect::to("/admin/login").into_response();
        }
    };

    // Get the collection definition for token expiry
    let def = match state.registry.get_collection(&collection) {
        Some(d) => d.clone(),
        None => return Redirect::to("/admin/login").into_response(),
    };

    let slug = collection;

    // Get session version
    let session_version = {
        let conn = match state.pool.get() {
            Ok(c) => c,
            Err(_) => return Redirect::to("/admin/login").into_response(),
        };
        query::get_session_version(&conn, &slug, &user.id).unwrap_or(0)
    };

    let user_email = user
        .fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let expiry = def
        .auth
        .as_ref()
        .map(|a| a.token_expiry)
        .unwrap_or(state.config.auth.token_expiry);

    let claims = match ClaimsBuilder::new(user.id.clone(), Slug::new(&slug))
        .email(user_email)
        .exp((chrono::Utc::now().timestamp() as u64) + expiry)
        .session_version(session_version)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Auth callback claims error: {}", e);
            return Redirect::to("/admin/login").into_response();
        }
    };

    let token = match state.token_provider.create_token(&claims) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Auth callback token error: {}", e);
            return Redirect::to("/admin/login").into_response();
        }
    };

    // Clear rate limit on success
    state.ip_login_limiter.clear(&ip);

    let cookies = session_cookies(&token, expiry, claims.exp, state.config.admin.dev_mode);
    let mut response = Redirect::to("/admin").into_response();

    for cookie in cookies {
        response.headers_mut().append(
            axum::http::header::SET_COOKIE,
            cookie.parse().expect("cookie header is valid ASCII"),
        );
    }

    response
}
