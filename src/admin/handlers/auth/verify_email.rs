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
    // Rate limit by IP to prevent brute-forcing verification tokens.
    // Uses the dedicated forgot-password IP limiter (not login limiter) to avoid
    // verification failures blocking legitimate login attempts from the same IP.
    let ip = client_ip(&headers, &addr, state.config.server.trust_proxy);
    if state.ip_forgot_password_limiter.is_blocked(&ip) {
        return Redirect::to("/admin/login");
    }

    let pool = state.pool.clone();
    let registry = state.registry.clone();
    let token = query.token;

    let result = task::spawn_blocking(move || {
        let mut conn = pool.get()?;
        let tx = conn.transaction()?;

        for def in registry.collections.values() {
            if !def.is_auth_collection() {
                continue;
            }

            if !def.auth.as_ref().is_some_and(|a| a.verify_email) {
                continue;
            }

            if let Some((user, exp)) =
                query::find_by_verification_token(&tx, &def.slug, def, &token)?
            {
                if Utc::now().timestamp() >= exp {
                    // Clean up expired token
                    if let Err(e) = query::clear_verification_token(&tx, &def.slug, &user.id) {
                        tracing::warn!("Failed to clear expired verification token: {}", e);
                    }
                    tx.commit()?;
                    return Ok(false);
                }

                // Block verification for locked accounts (consistent with reset_password)
                if query::is_locked(&tx, &def.slug, &user.id)? {
                    query::clear_verification_token(&tx, &def.slug, &user.id)?;
                    tx.commit()?;
                    return Ok(false);
                }

                query::mark_verified(&tx, &def.slug, &user.id)?;
                tx.commit()?;

                return Ok(true);
            }
        }

        Ok::<_, Error>(false)
    })
    .await;

    match result {
        Ok(Ok(true)) => Redirect::to("/admin/login?success=success_email_verified"),
        Ok(Ok(false)) => {
            // Invalid or expired token — record rate-limit failure
            state.ip_forgot_password_limiter.record_failure(&ip);
            Redirect::to("/admin/login")
        }
        Ok(Err(e)) => {
            // Internal error — log but don't penalize IP
            tracing::error!("Email verification error: {}", e);
            Redirect::to("/admin/login")
        }
        Err(e) => {
            // Task join error — log but don't penalize IP
            tracing::error!("Email verification task error: {}", e);
            Redirect::to("/admin/login")
        }
    }
}
