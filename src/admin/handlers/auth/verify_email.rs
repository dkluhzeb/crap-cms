use std::net::SocketAddr;

use anyhow::Error;
use axum::{
    extract::{ConnectInfo, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect},
};
use chrono::Utc;
use tokio::task;

use super::{VerifyEmailQuery, client_ip};
use crate::{admin::AdminState, db::query};

/// GET /admin/verify-email?token=xxx — validate token, mark verified, redirect.
pub async fn verify_email(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<VerifyEmailQuery>,
) -> impl IntoResponse {
    // Rate limit by IP to prevent brute-forcing verification tokens
    let ip = client_ip(&headers, &addr, state.config.server.trust_proxy);
    if state.ip_login_limiter.is_blocked(&ip) {
        return Redirect::to("/admin/login");
    }

    let pool = state.pool.clone();
    let registry = state.registry.clone();
    let token = query.token;

    let result = task::spawn_blocking(move || {
        let conn = pool.get()?;

        for def in registry.collections.values() {
            if !def.is_auth_collection() {
                continue;
            }

            if !def.auth.as_ref().is_some_and(|a| a.verify_email) {
                continue;
            }

            if let Some((user, exp)) =
                query::find_by_verification_token(&conn, &def.slug, def, &token)?
            {
                if Utc::now().timestamp() >= exp {
                    // Clean up expired token
                    let _ = query::clear_verification_token(&conn, &def.slug, &user.id);
                    return Ok(false);
                }

                query::mark_verified(&conn, &def.slug, &user.id)?;

                return Ok(true);
            }
        }

        Ok::<_, Error>(false)
    })
    .await;

    match result {
        Ok(Ok(true)) => Redirect::to("/admin/login?success=success_email_verified"),
        _ => {
            // Record failure for invalid/expired tokens to throttle brute-force attempts
            state.ip_login_limiter.record_failure(&ip);
            Redirect::to("/admin/login")
        }
    }
}
