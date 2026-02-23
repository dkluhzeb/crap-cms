//! Static asset serving with config-dir overlay over compiled-in defaults.

use axum::{
    body::Body,
    handler::HandlerWithoutStateExt,
    http::{header, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use include_dir::{include_dir, Dir};
use std::path::Path as StdPath;
use tower_http::services::ServeDir;

static STATIC_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/static");

/// Create a service that checks config_dir/static/ first, then falls back to embedded.
pub fn overlay_service(config_dir: &StdPath) -> Router {
    let config_static = config_dir.join("static");

    if config_static.exists() {
        // Config dir static files first, embedded fallback for anything not found
        let serve_dir = ServeDir::new(config_static)
            .fallback(get(embedded_static).into_service());
        Router::new()
            .fallback_service(serve_dir)
    } else {
        Router::new()
            .fallback(get(embedded_static))
    }
}

async fn embedded_static(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let mime_type = mime_guess::from_path(path).first_or_text_plain();

    match STATIC_DIR.get_file(path) {
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::NOT_FOUND.into_response()),
        Some(file) => {
            let content_type = HeaderValue::from_str(mime_type.as_ref())
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(file.contents().to_vec()))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}
