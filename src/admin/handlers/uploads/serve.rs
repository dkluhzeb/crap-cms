//! Serves uploaded files with access-control-aware caching.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{
        HeaderValue, Request, StatusCode,
        header::{
            ACCEPT, AUTHORIZATION, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_SECURITY_POLICY,
            CONTENT_TYPE, COOKIE, IF_MODIFIED_SINCE, IF_NONE_MATCH, RANGE, VARY,
        },
    },
    response::{IntoResponse, Response},
};
use tokio::task;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use std::path;

use crate::{
    admin::{
        AdminState,
        server::{extract_cookie, load_auth_user},
    },
    core::{AuthUser, auth::validate_token},
    db::AccessResult,
};

/// Check if a path segment contains traversal characters.
fn has_path_traversal(segment: &str) -> bool {
    segment.contains("..") || segment.contains('/') || segment.contains('\\')
}

/// Check collection read access, returning the cache policy to use.
/// Returns `None` if access is denied.
async fn check_upload_access(
    state: &AdminState,
    collection_slug: &str,
    auth_user: Option<AuthUser>,
) -> Option<&'static str> {
    let access_ref = state
        .registry
        .get_collection(collection_slug)
        .and_then(|def| def.access.read.clone());

    let Some(func_ref) = access_ref else {
        return Some("public, max-age=31536000, immutable");
    };

    let user_doc = auth_user.map(|u| u.user_doc);
    let pool = state.pool.clone();
    let hook_runner = state.hook_runner.clone();

    let access = task::spawn_blocking(move || {
        let conn = pool.get()?;
        hook_runner.check_access(Some(&func_ref), user_doc.as_ref(), None, None, &conn)
    })
    .await;

    let allowed = matches!(
        access,
        Ok(Ok(AccessResult::Allowed)) | Ok(Ok(AccessResult::Constrained(_)))
    );

    if allowed {
        Some("private, no-store")
    } else {
        None
    }
}

/// Serve an uploaded file, checking collection read access if configured.
///
/// Supports content negotiation for images: if the browser Accept header includes
/// `image/avif` or `image/webp`, and a variant file exists, the more
/// efficient format is served instead of the original.
pub async fn serve_upload(
    State(state): State<AdminState>,
    Path((collection_slug, filename)): Path<(String, String)>,
    request: Request<Body>,
) -> Response {
    if has_path_traversal(&collection_slug) || has_path_traversal(&filename) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let accept = request
        .headers()
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let auth_user = extract_auth_user(&request, &state);

    let Some(cache_control) = check_upload_access(&state, &collection_slug, auth_user).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    serve_file(
        &state,
        &collection_slug,
        &filename,
        cache_control,
        accept.contains("image/avif"),
        accept.contains("image/webp"),
        request,
    )
    .await
}

/// Try to authenticate from a raw token string (cookie value or Bearer token).
fn auth_from_token(token: &str, state: &AdminState) -> Option<AuthUser> {
    let claims = validate_token(token, state.jwt_secret.as_ref()).ok()?;
    load_auth_user(&state.pool, &state.registry, &claims, &state.config.locale)
}

fn extract_auth_user(request: &Request<Body>, state: &AdminState) -> Option<AuthUser> {
    let cookie_header = request
        .headers()
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if let Some(token) = extract_cookie(cookie_header, "crap_session")
        && let Some(user) = auth_from_token(token, state)
    {
        return Some(user);
    }

    let auth_header = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if let Some(token) = auth_header.strip_prefix("Bearer ") {
        return auth_from_token(token, state);
    }

    None
}

async fn serve_file(
    state: &AdminState,
    collection_slug: &str,
    filename: &str,
    cache_control: &str,
    accepts_avif: bool,
    accepts_webp: bool,
    original_request: Request<Body>,
) -> Response {
    let storage = &*state.storage;

    // Extract conditional headers from original request for ServeFile forwarding
    let conditional_headers = extract_conditional_headers(&original_request);

    // Content negotiation: try serving a more efficient format variant
    for (variant_name, variant_mime) in negotiate_variants(filename, accepts_avif, accepts_webp) {
        let variant_key = format!("{}/{}", collection_slug, variant_name);

        if let Some(local_path) = storage.local_path(&variant_key) {
            if local_path.exists() {
                let req = build_serve_request(&conditional_headers);

                return serve_with_headers(&local_path, req, cache_control, true, variant_mime)
                    .await;
            }
        } else if let Ok(data) = storage.get(&variant_key) {
            return serve_bytes(data, cache_control, true, variant_mime);
        }
    }

    // Serve the original file
    let original_key = format!("{}/{}", collection_slug, filename);

    let requested_mime = mime_guess::from_path(filename)
        .first_or_octet_stream()
        .to_string();
    let is_image = requested_mime.starts_with("image/");

    if let Some(local_path) = storage.local_path(&original_key) {
        if !local_path.exists() {
            return StatusCode::NOT_FOUND.into_response();
        }

        let req = build_serve_request(&conditional_headers);
        serve_with_headers(&local_path, req, cache_control, is_image, &requested_mime).await
    } else {
        match storage.get(&original_key) {
            Ok(data) => serve_bytes(data, cache_control, is_image, &requested_mime),
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        }
    }
}

/// Given a filename and accepted formats, return candidate variant filenames to try.
/// Returns `(variant_filename, mime_type)` pairs in preference order (AVIF first, then WebP).
/// Only returns candidates for image files.
fn negotiate_variants(
    filename: &str,
    accepts_avif: bool,
    accepts_webp: bool,
) -> Vec<(String, &'static str)> {
    let mime = mime_guess::from_path(filename)
        .first_or_octet_stream()
        .to_string();

    if !mime.starts_with("image/") {
        return Vec::new();
    }

    let stem = match filename.rfind('.') {
        Some(pos) if pos > 0 => &filename[..pos],
        _ => return Vec::new(),
    };

    let mut variants = Vec::new();
    if accepts_avif {
        variants.push((format!("{}.avif", stem), "image/avif"));
    }
    if accepts_webp {
        variants.push((format!("{}.webp", stem), "image/webp"));
    }
    variants
}

/// Conditional headers extracted from the original request, forwarded to ServeFile.
struct ConditionalHeaders {
    range: Option<HeaderValue>,
    if_none_match: Option<HeaderValue>,
    if_modified_since: Option<HeaderValue>,
}

fn extract_conditional_headers(req: &Request<Body>) -> ConditionalHeaders {
    ConditionalHeaders {
        range: req.headers().get(RANGE).cloned(),
        if_none_match: req.headers().get(IF_NONE_MATCH).cloned(),
        if_modified_since: req.headers().get(IF_MODIFIED_SINCE).cloned(),
    }
}

fn build_serve_request(headers: &ConditionalHeaders) -> Request<Body> {
    let mut builder = Request::builder().uri("/");

    if let Some(ref v) = headers.range {
        builder = builder.header(RANGE, v);
    }

    if let Some(ref v) = headers.if_none_match {
        builder = builder.header(IF_NONE_MATCH, v);
    }

    if let Some(ref v) = headers.if_modified_since {
        builder = builder.header(IF_MODIFIED_SINCE, v);
    }

    builder.body(Body::empty()).expect("static request builder")
}

/// Determine Content-Disposition for a file based on its MIME type.
///
/// Images (except SVG) are inline. SVGs and non-image files get attachment
/// to prevent stored XSS. If a filename is provided, it's included for
/// download naming (nanoid prefix is stripped).
fn content_disposition(mime: &str, filename: Option<&str>) -> String {
    if mime.starts_with("image/") && mime != "image/svg+xml" {
        return "inline".to_string();
    }

    let original = filename
        .and_then(|n| n.find('_').map(|pos| &n[pos + 1..]))
        .filter(|n| !n.is_empty());

    match original {
        Some(name) => format!("attachment; filename=\"{}\"", name.replace('"', "_")),
        None => "attachment".to_string(),
    }
}

/// Apply shared security/caching headers to a response.
fn apply_response_headers(response: &mut Response, cache_control: &str, mime: &str, varied: bool) {
    response.headers_mut().insert(
        CACHE_CONTROL,
        cache_control.parse().expect("valid cache-control"),
    );

    if mime == "image/svg+xml" {
        response.headers_mut().insert(
            CONTENT_SECURITY_POLICY,
            "sandbox; default-src 'none'".parse().expect("valid csp"),
        );
    }

    if varied {
        response
            .headers_mut()
            .insert(VARY, "Accept".parse().expect("valid vary"));
    }
}

/// Serve a file via `tower_http::services::ServeFile` with custom headers.
/// Provides Range, ETag, Last-Modified, and conditional GET support for free.
async fn serve_with_headers(
    path: &path::Path,
    request: Request<Body>,
    cache_control: &str,
    varied: bool,
    mime: &str,
) -> Response {
    let service = ServeFile::new(path);
    let mut response = match service.oneshot(request).await {
        Ok(r) => r.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let filename = path.file_name().and_then(|n| n.to_str());
    let disposition = content_disposition(mime, filename);

    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        disposition.parse().expect("valid disposition"),
    );

    apply_response_headers(&mut response, cache_control, mime, varied);
    response
}

/// Build a response from in-memory bytes (for non-local storage backends).
fn serve_bytes(data: Vec<u8>, cache_control: &str, varied: bool, mime: &str) -> Response {
    let disposition = content_disposition(mime, None);

    let builder = Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, mime)
        .header(CACHE_CONTROL, cache_control)
        .header(CONTENT_DISPOSITION, disposition);

    let mut response = builder
        .body(Body::from(data))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());

    apply_response_headers(&mut response, cache_control, mime, varied);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use std::fs;

    #[test]
    fn negotiate_no_accept_returns_empty() {
        let variants = negotiate_variants("photo.jpg", false, false);
        assert!(variants.is_empty());
    }

    #[test]
    fn negotiate_avif_for_image() {
        let variants = negotiate_variants("photo.jpg", true, false);
        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0], ("photo.avif".to_string(), "image/avif"));
    }

    #[test]
    fn negotiate_webp_for_image() {
        let variants = negotiate_variants("photo.jpg", false, true);
        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0], ("photo.webp".to_string(), "image/webp"));
    }

    #[test]
    fn negotiate_prefers_avif_over_webp() {
        let variants = negotiate_variants("photo.jpg", true, true);
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].1, "image/avif");
        assert_eq!(variants[1].1, "image/webp");
    }

    #[test]
    fn negotiate_non_image_returns_empty() {
        let variants = negotiate_variants("document.pdf", true, true);
        assert!(variants.is_empty());
    }

    #[test]
    fn negotiate_no_extension_returns_empty() {
        let variants = negotiate_variants("noext", true, true);
        assert!(variants.is_empty());
    }

    #[test]
    fn negotiate_preserves_stem_with_underscores() {
        let variants = negotiate_variants("abc123_photo_thumbnail.jpg", true, true);
        assert_eq!(variants[0].0, "abc123_photo_thumbnail.avif");
        assert_eq!(variants[1].0, "abc123_photo_thumbnail.webp");
    }

    #[test]
    fn negotiate_png_image() {
        let variants = negotiate_variants("icon.png", false, true);
        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0], ("icon.webp".to_string(), "image/webp"));
    }

    #[tokio::test]
    async fn serve_with_headers_image_disposition_inline() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.png");
        fs::write(&path, b"fake png").unwrap();
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let resp = serve_with_headers(&path, req, "public", false, "image/png").await;
        let disposition = resp
            .headers()
            .get(CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(disposition, "inline");
    }

    #[tokio::test]
    async fn serve_with_headers_pdf_disposition_attachment() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.pdf");
        fs::write(&path, b"fake pdf").unwrap();
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let resp = serve_with_headers(&path, req, "public", false, "application/pdf").await;
        let disposition = resp
            .headers()
            .get(CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(disposition, "attachment");
    }

    #[tokio::test]
    async fn serve_with_headers_varied_sets_vary() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.jpg");
        fs::write(&path, b"fake jpg").unwrap();
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let resp = serve_with_headers(&path, req, "public", true, "image/jpeg").await;
        assert_eq!(resp.headers().get(VARY).unwrap(), "Accept");
    }

    #[tokio::test]
    async fn serve_with_headers_no_vary_when_not_set() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, b"hello").unwrap();
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let resp = serve_with_headers(&path, req, "no-cache", false, "text/plain").await;
        // ServeFile may set Vary internally, but we don't set it
        assert!(!resp.headers().get_all(VARY).iter().any(|v| v == "Accept"));
    }

    #[tokio::test]
    async fn serve_with_headers_svg_attachment_and_csp() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.svg");
        fs::write(&path, b"<svg></svg>").unwrap();
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let resp = serve_with_headers(&path, req, "public", false, "image/svg+xml").await;
        let disposition = resp
            .headers()
            .get(CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(disposition, "attachment");
        let csp = resp
            .headers()
            .get(CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(csp, "sandbox; default-src 'none'");
    }

    #[test]
    fn extract_conditional_headers_captures_range() {
        let req = Request::builder()
            .uri("/")
            .header(RANGE, "bytes=0-99")
            .header(IF_NONE_MATCH, "\"abc\"")
            .body(Body::empty())
            .unwrap();
        let headers = extract_conditional_headers(&req);
        assert_eq!(headers.range.unwrap().to_str().unwrap(), "bytes=0-99");
        assert_eq!(headers.if_none_match.unwrap().to_str().unwrap(), "\"abc\"");
        assert!(headers.if_modified_since.is_none());
    }

    #[test]
    fn build_serve_request_forwards_headers() {
        let cond = ConditionalHeaders {
            range: Some("bytes=0-99".parse().unwrap()),
            if_none_match: None,
            if_modified_since: None,
        };
        let req = build_serve_request(&cond);
        assert_eq!(req.headers().get(RANGE).unwrap(), "bytes=0-99");
        assert!(req.headers().get(IF_NONE_MATCH).is_none());
    }
}
