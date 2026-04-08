use anyhow::{self, bail};
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::auth::{append_cookies, create_session_token, session_cookies},
    },
    core::auth::Claims,
    db::{DbPool, query::is_valid_identifier},
    service,
};

/// Verify that the user still exists, is not locked, and return the current session version.
///
/// Returns `Ok((locked, session_version))` on success.
fn check_session_status(pool: &DbPool, slug: &str, user_id: &str) -> anyhow::Result<(bool, u64)> {
    if !is_valid_identifier(slug) {
        bail!("Invalid collection slug");
    }

    let conn = pool.get()?;

    // Verify user still exists — is_locked and get_session_version both
    // return defaults (false/0) for missing rows, so a deleted user would
    // silently pass all checks and refresh their session indefinitely.
    if !service::auth::user_exists(&conn, slug, user_id).map_err(|e| e.into_anyhow())? {
        bail!("User no longer exists");
    }

    let locked = service::auth::is_locked(&conn, slug, user_id).map_err(|e| e.into_anyhow())?;
    let session_version =
        service::auth::get_session_version(&conn, slug, user_id).map_err(|e| e.into_anyhow())?;

    Ok((locked, session_version))
}

/// POST /admin/api/session-refresh — issue a fresh JWT if the current one is still valid.
pub async fn session_refresh(State(state): State<AdminState>, request: Request<Body>) -> Response {
    let claims = match request.extensions().get::<Claims>() {
        Some(c) => c.clone(),
        None => return StatusCode::UNAUTHORIZED.into_response(),
    };

    let pool = state.pool.clone();
    let slug = claims.collection.clone();
    let user_id = claims.sub.clone();

    let check_result =
        task::spawn_blocking(move || check_session_status(&pool, &slug, &user_id)).await;

    let session_version = match check_result {
        Ok(Ok((true, _))) => return StatusCode::UNAUTHORIZED.into_response(),
        Ok(Ok((false, sv))) => sv,
        Ok(Err(e)) => {
            error!("Session refresh check: {}", e);

            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Err(e) => {
            error!("Session refresh task error: {}", e);

            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Reject tokens with stale session version (password was changed)
    if claims.session_version != session_version {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let session = match create_session_token(
        &state,
        claims.sub.to_string(),
        &claims.collection,
        claims.email,
        session_version,
    ) {
        Ok(s) => s,
        Err(e) => {
            error!("Session refresh: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let cookies = session_cookies(
        &session.token,
        session.expiry,
        session.exp,
        state.config.admin.dev_mode,
    );
    let mut response = StatusCode::NO_CONTENT.into_response();

    append_cookies(&mut response, &cookies);

    response
}
