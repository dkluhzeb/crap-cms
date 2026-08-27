use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Error;
use axum::{
    extract::{ConnectInfo, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect},
};
use tokio::task;
use tracing::error;

use crate::core::collection::Auth;
use crate::{
    admin::{
        AdminState,
        handlers::{
            auth::{VerifyEmailQuery, client_ip, scoped_limiter},
            shared::paths,
        },
    },
    core::Registry,
    db::DbPool,
    service::{
        ServiceContext, auth::consume_verification_token as service_consume_verification_token,
    },
};

/// Find a verification token across all auth collections, validate it,
/// and mark the user as verified inside a transaction.
///
/// Returns `true` if the email was successfully verified, `false` if the
/// token is invalid, expired, or the account is locked.
fn consume_verification_token(
    pool: &DbPool,
    registry: &Registry,
    token: &str,
) -> Result<bool, Error> {
    let mut conn = pool.write()?;
    // SELECT-then-UPDATE (find token row, then mark verified): take a write lock
    // up front. A DEFERRED tx would risk `SQLITE_BUSY_SNAPSHOT` under concurrent
    // writers — same reasoning as the gRPC verify-email path.
    let tx = conn.transaction_immediate()?;

    for def in registry.collections.values() {
        if !def.is_auth_collection() {
            continue;
        }

        if !def.auth.as_ref().is_some_and(Auth::requires_verify_email) {
            continue;
        }

        let ctx = ServiceContext::collection(&def.slug, def).conn(&tx).build();

        if service_consume_verification_token(&ctx, token)? {
            tx.commit()?;
            return Ok(true);
        }
    }

    Ok(false)
}

/// GET /admin/verify-email?token=xxx — validate token, mark verified, redirect.
pub async fn verify_email(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<VerifyEmailQuery>,
) -> impl IntoResponse {
    let ip = client_ip(&headers, &addr, &state.config.server);

    // Rate limit by IP to prevent brute-forcing verification tokens. Atomically
    // record this attempt and bail if it puts the IP over the threshold — one
    // backend op, closing the check-then-record race the old `is_blocked` +
    // `record_failure` split left open. Every attempt counts (the same idiom as
    // login and forgot-password), so a transient internal error counts too;
    // that is acceptable for a high-entropy token endpoint and strictly safer
    // than refunding (no induce-error-to-refund vector). Uses its OWN per-IP
    // keyspace (not the shared forgot-password limiter) so a burst of
    // verification attempts can't exhaust the budget a legitimate password
    // reset from the same IP needs.
    let ip_verify_limiter = scoped_limiter(
        &state,
        "ip_verify_email",
        state.config.auth.max_ip_login_attempts,
        state.config.auth.forgot_password_window_seconds,
    );
    if ip_verify_limiter.check_and_block(&ip) {
        return Redirect::to(paths::LOGIN);
    }

    let pool = state.infra.pool.clone();
    let registry = Arc::clone(&state.infra.registry);
    let token = query.token;

    let result =
        task::spawn_blocking(move || consume_verification_token(&pool, &registry, &token)).await;

    match result {
        Ok(Ok(true)) => Redirect::to(&paths::login_with_success("success_email_verified")),
        Ok(Ok(false)) => Redirect::to(paths::LOGIN),
        Ok(Err(e)) => {
            error!("Email verification error: {}", e);
            Redirect::to(paths::LOGIN)
        }
        Err(e) => {
            error!("Email verification task error: {}", e);
            Redirect::to(paths::LOGIN)
        }
    }
}
