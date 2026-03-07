//! Serves uploaded files with access-control-aware caching.

use axum::{
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::admin::AdminState;
use crate::admin::server::{extract_cookie, load_auth_user};
use crate::core::auth;
use crate::db::query::AccessResult;

/// Serve an uploaded file, checking collection read access if configured.
///
/// Supports content negotiation for images: if the browser Accept header includes
/// `image/avif` or `image/webp`, and a variant file exists on disk, the more
/// efficient format is served instead of the original.
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

    // Parse Accept header for content negotiation
    let accept = request.headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let accepts_avif = accept.contains("image/avif");
    let accepts_webp = accept.contains("image/webp");

    // Look up collection access.read
    let access_read = state.registry.get_collection(&collection_slug)
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

        let allowed = matches!(access, Ok(Ok(AccessResult::Allowed)) | Ok(Ok(AccessResult::Constrained(_))));

        if !allowed {
            return StatusCode::NOT_FOUND.into_response();
        }

        // Serve with private cache headers
        return serve_file(
            &state, &collection_slug, &filename,
            "private, no-store", accepts_avif, accepts_webp,
        ).await;
    }

    // Public: no access.read set
    serve_file(
        &state, &collection_slug, &filename,
        "public, max-age=31536000, immutable", accepts_avif, accepts_webp,
    ).await
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
            if let Some(auth_user) = load_auth_user(&state.pool, &state.registry, &claims, &state.config.locale) {
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
            if let Some(auth_user) = load_auth_user(&state.pool, &state.registry, &claims, &state.config.locale) {
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
    accepts_avif: bool,
    accepts_webp: bool,
) -> Response {
    let upload_dir = state.config_dir.join("uploads").join(collection_slug);

    // Content negotiation: try serving a more efficient format variant
    for (variant_name, variant_mime) in negotiate_variants(filename, accepts_avif, accepts_webp) {
        let variant_path = upload_dir.join(&variant_name);
        if let Ok(bytes) = tokio::fs::read(&variant_path).await {
            return serve_bytes(bytes, variant_mime, cache_control, true);
        }
    }

    // Serve the original file
    let file_path = upload_dir.join(filename);
    let bytes = match tokio::fs::read(&file_path).await {
        Ok(b) => b,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let requested_mime = mime_guess::from_path(filename)
        .first_or_octet_stream()
        .to_string();
    // Always set Vary: Accept for images so caches don't serve the wrong format
    let is_image = requested_mime.starts_with("image/");
    serve_bytes(bytes, &requested_mime, cache_control, is_image)
}

/// Given a filename and accepted formats, return candidate variant filenames to try.
/// Returns `(variant_filename, mime_type)` pairs in preference order (AVIF first, then WebP).
/// Only returns candidates for image files.
fn negotiate_variants(filename: &str, accepts_avif: bool, accepts_webp: bool) -> Vec<(String, &'static str)> {
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

fn serve_bytes(bytes: Vec<u8>, content_type: &str, cache_control: &str, varied: bool) -> Response {
    let len = bytes.len();
    // Content-Disposition: inline for images (render in browser), attachment for everything else
    let disposition = if content_type.starts_with("image/") {
        "inline".to_string()
    } else {
        "attachment".to_string()
    };
    let mut builder = axum::http::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(header::CACHE_CONTROL, cache_control)
        .header(header::CONTENT_DISPOSITION, disposition);

    // Vary: Accept tells caches that the response depends on the Accept header
    if varied {
        builder = builder.header(header::VARY, "Accept");
    }

    builder.body(axum::body::Body::from(bytes))
        .expect("in-memory body builder")
        .into_response()
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

    #[test]
    fn serve_bytes_image_content_disposition_inline() {
        let resp = serve_bytes(vec![1, 2, 3], "image/png", "public", false);
        let disposition = resp.headers().get(header::CONTENT_DISPOSITION).unwrap().to_str().unwrap();
        assert_eq!(disposition, "inline");
    }

    #[test]
    fn serve_bytes_pdf_content_disposition_attachment() {
        let resp = serve_bytes(vec![1, 2, 3], "application/pdf", "public", false);
        let disposition = resp.headers().get(header::CONTENT_DISPOSITION).unwrap().to_str().unwrap();
        assert_eq!(disposition, "attachment");
    }

    #[test]
    fn serve_bytes_sets_content_type_and_length() {
        let data = vec![0u8; 42];
        let resp = serve_bytes(data, "text/plain", "no-cache", false);
        assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), "text/plain");
        assert_eq!(resp.headers().get(header::CONTENT_LENGTH).unwrap(), "42");
        assert!(resp.headers().get(header::VARY).is_none());
    }

    #[test]
    fn serve_bytes_varied_sets_vary_header() {
        let resp = serve_bytes(vec![1], "image/jpeg", "public", true);
        assert_eq!(resp.headers().get(header::VARY).unwrap(), "Accept");
    }
}
