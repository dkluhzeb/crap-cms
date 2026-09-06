//! Serves uploaded files with access-control-aware caching.

use std::sync::Arc;

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

use crate::admin::handlers::shared::response::on_blocking_section;
use crate::{
    admin::{
        AdminState,
        handlers::auth::SESSION_COOKIE,
        server::{extract_cookie, load_auth_user},
    },
    config::LocaleConfig,
    core::{
        AuthUser, CollectionDefinition, Document,
        upload::{SharedStorage, StorageNotFound, served_url, verify_upload_sig},
    },
    db::{DbPool, Filter, FilterClause, FilterOp, FindQuery, LocaleContext},
    hooks::HookRunner,
    service::{FindDocumentsInput, RunnerReadHooks, ServiceContext, find_documents},
};

/// Read a key off the async runtime. Local storage never reaches here (it
/// serves via `local_path` + `ServeFile`); S3 and custom backends do
/// blocking work — network I/O for S3, a pooled Lua VM call for custom —
/// so the read must run on a blocking thread, never on a tokio worker.
async fn storage_get_blocking(storage: &SharedStorage, key: String) -> anyhow::Result<Vec<u8>> {
    let storage = storage.clone();
    task::spawn_blocking(move || storage.get(&key)).await?
}

/// Check if a path segment contains traversal characters.
fn has_path_traversal(segment: &str) -> bool {
    segment.contains("..") || segment.contains('/') || segment.contains('\\')
}

/// Extract the signed-URL query parameters (`exp` + `sig`), ignoring any
/// other parameters. `None` unless both are present and `exp` parses.
fn signed_query_params(query: Option<&str>) -> Option<(i64, String)> {
    let query = query?;

    let mut exp: Option<i64> = None;
    let mut sig: Option<String> = None;

    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };

        match k {
            "exp" => exp = v.parse().ok(),
            "sig" => sig = Some(v.to_string()),
            _ => {}
        }
    }

    Some((exp?, sig?))
}

/// Signed-URL capability check: a valid `exp`/`sig` pair for this exact path
/// authorizes the request without re-running the per-document gate — the
/// authorization happened server-side at mint time (`crap.uploads.sign_url`).
/// Returns the cache policy (`private`, bounded by the remaining validity) on
/// success; `None` falls through to the normal cookie/Bearer resolution (an
/// invalid or expired signature grants nothing).
fn check_signed_url(
    state: &AdminState,
    collection_slug: &str,
    filename: &str,
    query: Option<&str>,
) -> Option<String> {
    let (exp, sig) = signed_query_params(query)?;

    let path = served_url(&format!("{collection_slug}/{filename}"));
    let now = chrono::Utc::now().timestamp();

    let secret: &str = state.config.auth.secret.as_ref();
    if !verify_upload_sig(secret, &path, exp, &sig, now) {
        return None;
    }

    // Cacheable by this viewer for the signature's remaining lifetime; never
    // by shared caches (the URL, not the viewer, is the capability).
    Some(format!("private, max-age={}", (exp - now).max(0)))
}

/// Owned inputs for [`upload_doc_visible`]'s `spawn_blocking` call.
struct UploadVisibilityInput {
    pool: DbPool,
    runner: HookRunner,
    def: Arc<CollectionDefinition>,
    slug: String,
    filename: String,
    user_doc: Option<Document>,
    locale_config: LocaleConfig,
}

/// `Cache-Control` for a provably-public upload: no access control at all
/// (`default_deny` off, no read hook, no draft/trash axis), and the content at
/// a nanoid-prefixed URL never changes, so cache hard and long. `31536000` =
/// one year, the conventional ceiling for `immutable` assets.
const CACHE_IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// `Cache-Control` for any access-gated upload. Whether a file serves depends
/// on the *viewer* (per-row read constraints, draft/trash access) and on
/// mutable document state — and access is not required to be monotonic, so an
/// anonymous-visible file is not provably visible to every caller. A shared
/// cache must therefore never store one viewer's resolution and replay it to
/// another, so these responses are never cached (`no-store`).
const CACHE_PRIVATE: &str = "private, no-store";

/// Whether the viewer may see the upload-collection **document** that owns this
/// file, applying the full content-view model — published ∪ draft (downgraded to
/// the viewer's access), with the read/draft hooks' row constraints matched
/// against the row, trashed rows excluded.
///
/// Every served file is owned by exactly one upload-collection document; the doc
/// carries the file's URL in `url` (original) or a `{size}[_fmt]_url` column
/// (variant). We reproduce the *stored* URL from the requested key via
/// `served_url` — the backend-agnostic proxy path the write path stores on every
/// backend — and match it against those columns. (Using the backend's
/// `public_url` here would 404 access-gated uploads on S3/custom, where the
/// direct object/CDN URL differs from the stored proxy path.) Fail-closed: any
/// error, an orphan (no owning doc), or a non-upload collection → not visible.
fn upload_doc_visible(input: &UploadVisibilityInput) -> bool {
    let Some(upload) = input.def.upload.as_ref() else {
        return false;
    };

    // URL-bearing columns the upload schema injects: `url` + `{size}[_fmt]_url`.
    let or_clauses: Vec<FilterClause> = {
        let key = format!("{}/{}", input.slug, input.filename);
        let requested_url = served_url(&key);

        upload
            .system_field_names()
            .into_iter()
            .filter(|n| n == "url" || n.ends_with("_url"))
            .map(|col| {
                FilterClause::Single(Filter {
                    field: col,
                    op: FilterOp::Equals(requested_url.clone()),
                })
            })
            .collect()
    };

    if or_clauses.is_empty() {
        return false;
    }

    let Ok(conn) = input.pool.get() else {
        return false;
    };

    let hooks = RunnerReadHooks::new(&input.runner, &conn, input.user_doc.as_ref(), None);
    let ctx = ServiceContext::collection(&input.slug, &input.def)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(input.user_doc.as_ref())
        .locale_config(Some(&input.locale_config))
        .build();

    let fq = FindQuery::builder()
        .filters(vec![FilterClause::or(or_clauses)])
        .limit(Some(1))
        .build();

    // A localized upload collection (e.g. a `caption` field marked `localized`)
    // stores that column per-locale (`caption__en`), so the SELECT needs a locale
    // context — without one it references the bare logical column (`caption`),
    // the query errors, and every file 404s. The default locale is sufficient:
    // the gate only resolves the owning row, not a specific translation.
    let locale_ctx = LocaleContext::from_locale_string(None, &input.locale_config)
        .ok()
        .flatten();

    // `include_drafts` lets a draft upload serve to a viewer with draft access;
    // the service downgrades to what each viewer may actually see.
    let find_input = FindDocumentsInput::builder(&fq)
        .include_drafts(true)
        .locale_ctx(locale_ctx.as_ref())
        .build();

    find_documents(&ctx, &find_input).is_ok_and(|r| !r.docs.is_empty())
}

/// Check that the viewer may read the upload document owning this file, returning
/// the cache policy. Returns `None` (→ 404) when no document the viewer can see
/// references the file — enforcing per-row constraints, draft, and trash on the
/// served bytes (the same model every other upload surface uses).
async fn check_upload_access(
    state: &AdminState,
    collection_slug: &str,
    filename: &str,
    auth_user: Option<AuthUser>,
) -> Option<&'static str> {
    let def = state
        .infra
        .registry
        .get_collection(collection_slug)?
        .clone();

    // Fast public path: only when "no read hook" genuinely means ALLOW — i.e.
    // `default_deny` is off — and there is no draft/trash axis (no status- or
    // viewer-dependent visibility). Then every file is unconditionally public
    // and can be served CDN-cacheable without a query. Under `default_deny`
    // (the secure-by-default), a hook-less collection denies reads, so fall
    // through to the access-resolving path, which 404s correctly.
    if !state.config.access.default_deny
        && def.access.read.is_none()
        && !def.has_drafts()
        && !def.soft_delete
    {
        return Some(CACHE_IMMUTABLE);
    }

    let input = UploadVisibilityInput {
        pool: state.infra.pool.clone(),
        runner: state.infra.hook_runner.clone(),
        def,
        slug: collection_slug.to_string(),
        filename: filename.to_string(),
        user_doc: auth_user.map(|u| u.user_doc),
        locale_config: state.config.locale.clone(),
    };

    let visible = task::spawn_blocking(move || upload_doc_visible(&input))
        .await
        .unwrap_or(false);

    visible.then_some(CACHE_PRIVATE)
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

    // Signed URL first: a valid capability serves without cookie/Bearer.
    // Anything less than valid falls through to the normal auth + gate.
    let signed_cache = check_signed_url(&state, &collection_slug, &filename, request.uri().query());

    let cache_control: String = if let Some(cache) = signed_cache {
        cache
    } else {
        // Token validation + user load is synchronous DB work — run
        // it on the blocking pool. (The access gate below is already async
        // / `spawn_blocking` internally.)
        let auth_user = on_blocking_section(|| extract_auth_user(&request, &state));

        let Some(cache) = check_upload_access(&state, &collection_slug, &filename, auth_user).await
        else {
            return StatusCode::NOT_FOUND.into_response();
        };

        cache.to_string()
    };

    serve_file(
        &state,
        &collection_slug,
        &filename,
        &cache_control,
        accept.contains("image/avif"),
        accept.contains("image/webp"),
        request,
    )
    .await
}

/// Try to authenticate from a raw token string (cookie value or Bearer token).
/// Routes through `state.infra.token_provider` (not the free `validate_token`
/// function) so a future swap of the JWT backend / signing key flows
/// here automatically — the older `jwt_secret`-direct form silently
/// 401'd everything in that scenario.
fn auth_from_token(token: &str, state: &AdminState) -> Option<AuthUser> {
    let claims = state.infra.token_provider.validate_token(token).ok()?;
    load_auth_user(
        &state.infra.pool,
        &state.infra.registry,
        &claims,
        &state.config.locale,
    )
}

fn extract_auth_user(request: &Request<Body>, state: &AdminState) -> Option<AuthUser> {
    let cookie_header = request
        .headers()
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if let Some(token) = extract_cookie(cookie_header, SESSION_COOKIE)
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
    let storage = &*state.infra.storage;

    // Extract conditional headers from original request for ServeFile forwarding
    let conditional_headers = extract_conditional_headers(&original_request);

    // Content negotiation: try serving a more efficient format variant
    for (variant_name, variant_mime) in negotiate_variants(filename, accepts_avif, accepts_webp) {
        let variant_key = format!("{collection_slug}/{variant_name}");

        if let Some(local_path) = storage.local_path(&variant_key) {
            if local_path.exists() {
                let req = build_serve_request(&conditional_headers);

                return serve_with_headers(&local_path, req, cache_control, true, variant_mime)
                    .await;
            }
        } else if let Ok(data) = storage_get_blocking(&state.infra.storage, variant_key).await {
            return serve_bytes(data, cache_control, true, variant_mime);
        }
    }

    // Serve the original file
    let original_key = format!("{collection_slug}/{filename}");

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
        match storage_get_blocking(&state.infra.storage, original_key).await {
            Ok(data) => serve_bytes(data, cache_control, is_image, &requested_mime),
            Err(e) if e.downcast_ref::<StorageNotFound>().is_some() => {
                StatusCode::NOT_FOUND.into_response()
            }
            // Transient / infrastructure failure (remote network error,
            // VM-pool-acquire timeout under load, …): a retryable 503, not
            // a cacheable 404 for a file that exists.
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
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
        variants.push((format!("{stem}.avif"), "image/avif"));
    }
    if accepts_webp {
        variants.push((format!("{stem}.webp"), "image/webp"));
    }
    variants
}

/// Conditional headers extracted from the original request, forwarded to `ServeFile`.
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
        Some(name) => {
            // Replace `"` (would break out of the quoted-string) and any control
            // character (`\r`/`\n`/… — an invalid `HeaderValue` byte). Extensions
            // are sanitized to alphanumerics on upload since alpha.10, but files
            // stored by older versions can still carry raw bytes — keep this
            // guard; without it the header build would panic.
            let safe: String = name
                .chars()
                .map(|c| if c == '"' || c.is_control() { '_' } else { c })
                .collect();
            format!("attachment; filename=\"{safe}\"")
        }
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
/// Provides Range, `ETag`, Last-Modified, and conditional GET support for free.
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

    // Never `.expect` on a data-derived header value. `content_disposition`
    // sanitizes control chars, but fall back to a bare `attachment` rather than
    // panic the request task if any unexpected byte survives.
    let disposition = disposition
        .parse()
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"));
    response
        .headers_mut()
        .insert(CONTENT_DISPOSITION, disposition);

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
    fn signed_query_params_extracts_pair() {
        let q = signed_query_params(Some("exp=1300&sig=abc123"));
        assert_eq!(q, Some((1300, "abc123".to_string())));
    }

    #[test]
    fn signed_query_params_ignores_extra_params() {
        let q = signed_query_params(Some("foo=1&exp=99&bar&sig=aa&baz=2"));
        assert_eq!(q, Some((99, "aa".to_string())));
    }

    #[test]
    fn signed_query_params_requires_both() {
        assert!(signed_query_params(Some("exp=1300")).is_none());
        assert!(signed_query_params(Some("sig=abc")).is_none());
        assert!(signed_query_params(Some("exp=notanum&sig=abc")).is_none());
        assert!(signed_query_params(None).is_none());
    }

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

    /// Regression: a stored filename carrying control bytes (the extension is
    /// not sanitized upstream, so a crafted upload can smuggle a CRLF here) must
    /// not produce an unparseable header value — previously `.expect` panicked
    /// the request task on any such file.
    #[test]
    fn content_disposition_sanitizes_control_chars() {
        let disposition = content_disposition("application/pdf", Some("nano123_photo.pd\r\nf"));
        assert_eq!(disposition, "attachment; filename=\"photo.pd__f\"");
        // Must be a valid header value (no panic on insert).
        assert!(disposition.parse::<HeaderValue>().is_ok());
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
