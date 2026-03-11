use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
};

use crate::admin::AdminState;
use crate::core::auth;
use crate::core::auth::ClaimsBuilder;
use crate::db::query;
use super::session_cookies;

/// POST /admin/api/session-refresh — issue a fresh JWT if the current one is still valid.
pub async fn session_refresh(
    State(state): State<AdminState>,
    request: axum::http::Request<axum::body::Body>,
) -> axum::response::Response {
    // Extract claims from request extensions (set by auth middleware)
    let claims = match request.extensions().get::<auth::Claims>() {
        Some(c) => c.clone(),
        None => return StatusCode::UNAUTHORIZED.into_response(),
    };

    // Check account is not locked
    let pool = state.pool.clone();
    let slug = claims.collection.clone();
    let user_id = claims.sub.clone();

    let locked = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        query::is_locked(&conn, &slug, &user_id)
    }).await;

    match locked {
        Ok(Ok(true)) => return StatusCode::UNAUTHORIZED.into_response(),
        Ok(Err(e)) => {
            tracing::error!("Session refresh lock check: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Err(e) => {
            tracing::error!("Session refresh task error: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        _ => {} // not locked, continue
    }

    // Compute fresh expiry (collection override or global config)
    let expiry = state.registry.get_collection(&claims.collection)
        .and_then(|def| def.auth.as_ref().map(|a| a.token_expiry))
        .unwrap_or(state.config.auth.token_expiry);

    let new_claims = ClaimsBuilder::new(claims.sub, claims.collection)
        .email(claims.email)
        .exp((chrono::Utc::now().timestamp() as u64) + expiry)
        .build();

    let token = match auth::create_token(&new_claims, &state.jwt_secret) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Session refresh token creation: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let cookies = session_cookies(&token, expiry, new_claims.exp, state.config.admin.dev_mode);
    let mut response = StatusCode::NO_CONTENT.into_response();
    for cookie in cookies {
        response.headers_mut().append(
            axum::http::header::SET_COOKIE,
            cookie.parse().expect("cookie header is valid ASCII"),
        );
    }
    response
}
