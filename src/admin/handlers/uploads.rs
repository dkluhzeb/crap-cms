use axum::{
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::admin::AdminState;
use crate::admin::server::{extract_cookie, load_auth_user};
use crate::core::auth;
use crate::db::query::AccessResult;

pub async fn serve_upload(
    State(state): State<AdminState>,
    Path((collection_slug, filename)): Path<(String, String)>,
    request: axum::http::Request<axum::body::Body>,
) -> Response {
    // Reject path traversal
    if collection_slug.contains("..") || collection_slug.contains('/')
        || collection_slug.contains('\\')
        || filename.contains("..") || filename.contains('/')
        || filename.contains('\\')
    {
        return StatusCode::NOT_FOUND.into_response();
    }

    // Look up collection access.read
    let access_read = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        reg.get_collection(&collection_slug)
            .map(|def| def.access.read.clone())
    };

    // Determine access ref: None means public (collection not found or no access.read)
    let access_ref = match access_read {
        Some(Some(ref r)) => Some(r.clone()),
        _ => None,
    };

    if let Some(func_ref) = access_ref {
        // Extract auth user from cookie or Bearer token
        let auth_user = extract_auth_user(&request, &state);

        let user_doc = auth_user.map(|u| u.user_doc);
        let pool = state.pool.clone();
        let hook_runner = state.hook_runner.clone();

        let access = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            hook_runner.check_access(
                Some(&func_ref),
                user_doc.as_ref(),
                None,
                None,
                &conn,
            )
        }).await;

        let allowed = match access {
            Ok(Ok(AccessResult::Allowed)) | Ok(Ok(AccessResult::Constrained(_))) => true,
            _ => false,
        };

        if !allowed {
            return StatusCode::NOT_FOUND.into_response();
        }

        // Serve with private cache headers
        return serve_file(&state, &collection_slug, &filename, "private, no-store").await;
    }

    // Public: no access.read set
    serve_file(&state, &collection_slug, &filename, "public, max-age=31536000, immutable").await
}

fn extract_auth_user(
    request: &axum::http::Request<axum::body::Body>,
    state: &AdminState,
) -> Option<auth::AuthUser> {
    // Try cookie first
    let cookie_header = request.headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if let Some(token) = extract_cookie(cookie_header, "crap_session") {
        if let Ok(claims) = auth::validate_token(token, &state.jwt_secret) {
            if let Some(auth_user) = load_auth_user(&state.pool, &state.registry, &claims) {
                return Some(auth_user);
            }
        }
    }

    // Try Bearer token
    let auth_header = request.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if let Some(token) = auth_header.strip_prefix("Bearer ") {
        if let Ok(claims) = auth::validate_token(token, &state.jwt_secret) {
            if let Some(auth_user) = load_auth_user(&state.pool, &state.registry, &claims) {
                return Some(auth_user);
            }
        }
    }

    None
}

async fn serve_file(
    state: &AdminState,
    collection_slug: &str,
    filename: &str,
    cache_control: &str,
) -> Response {
    let file_path = state.config_dir.join("uploads").join(collection_slug).join(filename);

    let bytes = match tokio::fs::read(&file_path).await {
        Ok(b) => b,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let mime = mime_guess::from_path(filename)
        .first_or_octet_stream()
        .to_string();

    let len = bytes.len();

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime),
            (header::CONTENT_LENGTH, len.to_string()),
            (header::CACHE_CONTROL, cache_control.to_string()),
        ],
        bytes,
    ).into_response()
}
