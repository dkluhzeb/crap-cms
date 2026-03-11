//! Static asset serving with config-dir overlay over compiled-in defaults.

use axum::{
    body::Body,
    handler::HandlerWithoutStateExt,
    http::{header, HeaderValue, StatusCode, Uri},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use include_dir::{include_dir, Dir};
use std::path::Path as StdPath;
use tower_http::services::ServeDir;

static STATIC_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/static");

/// Middleware that sets `Cache-Control: public, no-cache` on successful responses.
/// This ensures browsers always revalidate (using Last-Modified / If-Modified-Since
/// for config-dir files, or ETag / If-None-Match for embedded files).
async fn cache_control_middleware(
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    if response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, no-cache"),
        );
    }
    response
}

/// Create a service that checks config_dir/static/ first, then falls back to embedded.
pub fn overlay_service(config_dir: &StdPath) -> Router {
    let config_static = config_dir.join("static");

    let router = if config_static.exists() {
        // Config dir static files first, embedded fallback for anything not found
        let serve_dir = ServeDir::new(config_static).fallback(get(embedded_static).into_service());
        Router::new().fallback_service(serve_dir)
    } else {
        Router::new().fallback(get(embedded_static))
    };
    router.layer(middleware::from_fn(cache_control_middleware))
}

/// Build hash for cache-busting (changes every build when static/ or templates/ change).
static BUILD_HASH: &str = env!("BUILD_HASH");

async fn embedded_static(uri: Uri, headers: axum::http::HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    let mime_type = mime_guess::from_path(path).first_or_text_plain();

    match STATIC_DIR.get_file(path) {
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::NOT_FOUND.into_response()),
        Some(file) => {
            let etag_value = format!("\"{}\"", BUILD_HASH);

            // If the client sent a matching ETag, return 304.
            if let Some(inm) = headers.get(header::IF_NONE_MATCH) {
                if inm.as_bytes() == etag_value.as_bytes() {
                    return Response::builder()
                        .status(StatusCode::NOT_MODIFIED)
                        .header(header::ETAG, etag_value)
                        .body(Body::empty())
                        .unwrap_or_else(|_| StatusCode::NOT_MODIFIED.into_response());
                }
            }

            let content_type = HeaderValue::from_str(mime_type.as_ref())
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::ETAG, etag_value)
                .body(Body::from(file.contents().to_vec()))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}
