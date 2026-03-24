//! Serves uploaded files with access-control-aware caching.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderValue, Request, StatusCode, header},
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

/// Serve an uploaded file, checking collection read access if configured.
///
/// Supports content negotiation for images: if the browser Accept header includes
/// `image/avif` or `image/webp`, and a variant file exists on disk, the more
/// efficient format is served instead of the original.
pub async fn serve_upload(
    State(state): State<AdminState>,
    Path((collection_slug, filename)): Path<(String, String)>,
    request: Request<Body>,
) -> Response {
    // Reject path traversal
    if collection_slug.contains("..")
        || collection_slug.contains('/')
        || collection_slug.contains('\\')
        || filename.contains("..")
        || filename.contains('/')
        || filename.contains('\\')
    {
        return StatusCode::NOT_FOUND.into_response();
    }

    // Parse Accept header for content negotiation
    let accept = request
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let accepts_avif = accept.contains("image/avif");
    let accepts_webp = accept.contains("image/webp");

    // Look up collection access.read
    let access_read = state
        .registry
        .get_collection(&collection_slug)
        .map(|def| def.access.read.clone());

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

        let access = task::spawn_blocking(move || {
            let conn = pool.get()?;
            hook_runner.check_access(Some(&func_ref), user_doc.as_ref(), None, None, &conn)
        })
        .await;

        let allowed = matches!(
            access,
            Ok(Ok(AccessResult::Allowed)) | Ok(Ok(AccessResult::Constrained(_)))
        );

        if !allowed {
            return StatusCode::NOT_FOUND.into_response();
        }

        // Serve with private cache headers
        return serve_file(
            &state,
            &collection_slug,
            &filename,
            "private, no-store",
            accepts_avif,
            accepts_webp,
            request,
        )
        .await;
    }

    // Public: no access.read set
    serve_file(
        &state,
        &collection_slug,
        &filename,
        "public, max-age=31536000, immutable",
        accepts_avif,
        accepts_webp,
        request,
    )
    .await
}

fn extract_auth_user(request: &Request<Body>, state: &AdminState) -> Option<AuthUser> {
    // Try cookie first
    let cookie_header = request
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if let Some(token) = extract_cookie(cookie_header, "crap_session")
        && let Ok(claims) = validate_token(token, state.jwt_secret.as_ref())
        && let Some(auth_user) =
            load_auth_user(&state.pool, &state.registry, &claims, &state.config.locale)
    {
        return Some(auth_user);
    }

    // Try Bearer token
    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if let Some(token) = auth_header.strip_prefix("Bearer ")
        && let Ok(claims) = validate_token(token, state.jwt_secret.as_ref())
        && let Some(auth_user) =
            load_auth_user(&state.pool, &state.registry, &claims, &state.config.locale)
    {
        return Some(auth_user);
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
    let upload_dir = state.config_dir.join("uploads").join(collection_slug);

    // Extract conditional headers from original request for ServeFile forwarding
    let conditional_headers = extract_conditional_headers(&original_request);

    // Content negotiation: try serving a more efficient format variant
    for (variant_name, variant_mime) in negotiate_variants(filename, accepts_avif, accepts_webp) {
        let variant_path = upload_dir.join(&variant_name);
        if variant_path.exists() {
            let req = build_serve_request(&conditional_headers);
            return serve_with_headers(&variant_path, req, cache_control, true, variant_mime).await;
        }
    }

    // Serve the original file
    let file_path = upload_dir.join(filename);
    if !file_path.exists() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let requested_mime = mime_guess::from_path(filename)
        .first_or_octet_stream()
        .to_string();
    let is_image = requested_mime.starts_with("image/");
    let req = build_serve_request(&conditional_headers);
    serve_with_headers(&file_path, req, cache_control, is_image, &requested_mime).await
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
        range: req.headers().get(header::RANGE).cloned(),
        if_none_match: req.headers().get(header::IF_NONE_MATCH).cloned(),
        if_modified_since: req.headers().get(header::IF_MODIFIED_SINCE).cloned(),
    }
}

fn build_serve_request(headers: &ConditionalHeaders) -> Request<Body> {
    let mut builder = Request::builder().uri("/");

    if let Some(ref v) = headers.range {
        builder = builder.header(header::RANGE, v);
    }

    if let Some(ref v) = headers.if_none_match {
        builder = builder.header(header::IF_NONE_MATCH, v);
    }

    if let Some(ref v) = headers.if_modified_since {
        builder = builder.header(header::IF_MODIFIED_SINCE, v);
    }

    builder.body(Body::empty()).expect("static request builder")
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

    // Override Cache-Control for our needs
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        cache_control.parse().expect("valid cache-control"),
    );

    // SVGs get attachment + CSP sandbox to prevent stored XSS.
    // Non-image files include the original filename for proper download naming.
    let disposition = if mime.starts_with("image/") && mime != "image/svg+xml" {
        "inline".to_string()
    } else {
        // Extract original filename: strip nanoid prefix (format: "nanoid_originalname.ext")
        let original = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.find('_').map(|pos| &n[pos + 1..]))
            .filter(|n| !n.is_empty());
        match original {
            Some(name) => format!("attachment; filename=\"{}\"", name.replace('"', "_")),
            None => "attachment".to_string(),
        }
    };

    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        disposition.parse().expect("valid disposition"),
    );

    if mime == "image/svg+xml" {
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            "sandbox".parse().expect("valid csp"),
        );
    }

    if varied {
        response
            .headers_mut()
            .insert(header::VARY, "Accept".parse().expect("valid vary"));
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;

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
        std::fs::write(&path, b"fake png").unwrap();
        let req = axum::http::Request::builder()
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = serve_with_headers(&path, req, "public", false, "image/png").await;
        let disposition = resp
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(disposition, "inline");
    }

    #[tokio::test]
    async fn serve_with_headers_pdf_disposition_attachment() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.pdf");
        std::fs::write(&path, b"fake pdf").unwrap();
        let req = axum::http::Request::builder()
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = serve_with_headers(&path, req, "public", false, "application/pdf").await;
        let disposition = resp
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(disposition, "attachment");
    }

    #[tokio::test]
    async fn serve_with_headers_varied_sets_vary() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.jpg");
        std::fs::write(&path, b"fake jpg").unwrap();
        let req = axum::http::Request::builder()
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = serve_with_headers(&path, req, "public", true, "image/jpeg").await;
        assert_eq!(resp.headers().get(header::VARY).unwrap(), "Accept");
    }

    #[tokio::test]
    async fn serve_with_headers_no_vary_when_not_set() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");
        std::fs::write(&path, b"hello").unwrap();
        let req = axum::http::Request::builder()
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = serve_with_headers(&path, req, "no-cache", false, "text/plain").await;
        // ServeFile may set Vary internally, but we don't set it
        assert!(
            !resp
                .headers()
                .get_all(header::VARY)
                .iter()
                .any(|v| v == "Accept")
        );
    }

    #[tokio::test]
    async fn serve_with_headers_svg_attachment_and_csp() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.svg");
        std::fs::write(&path, b"<svg></svg>").unwrap();
        let req = axum::http::Request::builder()
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = serve_with_headers(&path, req, "public", false, "image/svg+xml").await;
        let disposition = resp
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(disposition, "attachment");
        let csp = resp
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(csp, "sandbox");
    }

    #[test]
    fn extract_conditional_headers_captures_range() {
        let req = axum::http::Request::builder()
            .uri("/")
            .header(header::RANGE, "bytes=0-99")
            .header(header::IF_NONE_MATCH, "\"abc\"")
            .body(axum::body::Body::empty())
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
        assert_eq!(req.headers().get(header::RANGE).unwrap(), "bytes=0-99");
        assert!(req.headers().get(header::IF_NONE_MATCH).is_none());
    }
}
