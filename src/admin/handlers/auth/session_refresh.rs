use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use tokio::task;

use super::session_cookies;
use crate::{
    admin::AdminState,
    core::auth::{Claims, ClaimsBuilder, create_token},
    db::query::{self, is_valid_identifier},
};

/// POST /admin/api/session-refresh — issue a fresh JWT if the current one is still valid.
pub async fn session_refresh(State(state): State<AdminState>, request: Request<Body>) -> Response {
    // Extract claims from request extensions (set by auth middleware)
    let claims = match request.extensions().get::<Claims>() {
        Some(c) => c.clone(),
        None => return StatusCode::UNAUTHORIZED.into_response(),
    };

    // Check account is not locked and fetch current session version
    let pool = state.pool.clone();
    let slug = claims.collection.clone();
    let user_id = claims.sub.clone();

    let check_result = task::spawn_blocking(move || {
        if !is_valid_identifier(&slug) {
            anyhow::bail!("Invalid collection slug");
        }

        let conn = pool.get()?;

        // Verify user still exists — is_locked and get_session_version both
        // return defaults (false/0) for missing rows, so a deleted user would
        // silently pass all checks and refresh their session indefinitely.
        if !query::user_exists(&conn, &slug, &user_id)? {
            anyhow::bail!("User no longer exists");
        }

        let locked = query::is_locked(&conn, &slug, &user_id)?;
        let session_version = query::get_session_version(&conn, &slug, &user_id)?;
        anyhow::Ok((locked, session_version))
    })
    .await;

    let session_version = match check_result {
        Ok(Ok((true, _))) => return StatusCode::UNAUTHORIZED.into_response(),
        Ok(Ok((false, sv))) => sv,
        Ok(Err(e)) => {
            tracing::error!("Session refresh check: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Err(e) => {
            tracing::error!("Session refresh task error: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Reject tokens with stale session version (password was changed)
    if claims.session_version != session_version {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Compute fresh expiry (collection override or global config)
    let expiry = state
        .registry
        .get_collection(&claims.collection)
        .and_then(|def| def.auth.as_ref().map(|a| a.token_expiry))
        .unwrap_or(state.config.auth.token_expiry);

    let new_claims = ClaimsBuilder::new(claims.sub, claims.collection)
        .email(claims.email)
        .exp((Utc::now().timestamp() as u64) + expiry)
        .session_version(session_version)
        .build();

    let token = match create_token(&new_claims, state.jwt_secret.as_ref()) {
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
            header::SET_COOKIE,
            cookie.parse().expect("cookie header is valid ASCII"),
        );
    }

    response
}
