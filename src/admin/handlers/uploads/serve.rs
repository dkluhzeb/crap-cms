//! Serves uploaded files with access-control-aware caching.

use std::{path, sync::Arc};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode, header::ACCEPT},
    response::{IntoResponse, Response},
};
use tokio::task;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::admin::handlers::shared::{db_error_status, response::on_blocking_section};
use crate::admin::handlers::uploads::{
    headers::{ConditionalHeaders, ServeHeaders, build_serve_request, extract_conditional_headers},
    remote::serve_remote,
};
use crate::{
    admin::{
        AdminState,
        server::{bearer_token, evaluate_admin_request, session_cookie_token},
    },
    config::LocaleConfig,
    core::{
        AuthUser, CollectionDefinition, Document,
        upload::{served_url, verify_upload_sig},
    },
    db::{DbPool, Filter, FilterClause, FilterOp, FindQuery, LocaleContext},
    hooks::HookRunner,
    service::{
        FindDocumentsInput, RunnerReadHooks, ServiceContext, ServiceError, auth::Resolution,
        find_documents,
    },
};

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
/// direct object/CDN URL differs from the stored proxy path.) An orphan (no
/// owning doc), a non-upload collection, or a refusing or failing read → not
/// visible; a database error is returned, so the viewer is told to retry rather
/// than that the file doesn't exist.
fn upload_doc_visible(input: &UploadVisibilityInput) -> anyhow::Result<bool> {
    let Some(upload) = input.def.upload.as_ref() else {
        return Ok(false);
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
        return Ok(false);
    }

    let conn = input.pool.get()?;

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
    let locale_ctx = LocaleContext::default_for(&input.locale_config);

    // `include_drafts` lets a draft upload serve to a viewer with draft access;
    // the service downgrades to what each viewer may actually see.
    let find_input = FindDocumentsInput::builder(&fq)
        .include_drafts(true)
        .locale_ctx(locale_ctx.as_ref())
        .build();

    visible_or_retry(find_documents(&ctx, &find_input), |r| !r.docs.is_empty())
}

/// Whether a read for the owning document shows it: a transient database error
/// is returned, so the viewer is told to retry; any other failure — a refusing
/// rule, a failing hook — means not visible.
fn visible_or_retry<T>(
    result: Result<T, ServiceError>,
    shows: impl FnOnce(T) -> bool,
) -> anyhow::Result<bool> {
    match result {
        Ok(found) => Ok(shows(found)),
        Err(ServiceError::Transient(e)) => Err(e),
        Err(_) => Ok(false),
    }
}

/// Check that the viewer may read the upload document owning this file, returning
/// the cache policy. Returns `None` (→ 404) when no document the viewer can see
/// references the file — enforcing per-row constraints, draft, and trash on the
/// served bytes (the same model every other upload surface uses) — and the
/// status to answer when the database can't say.
async fn check_upload_access(
    state: &AdminState,
    collection_slug: &str,
    filename: &str,
    auth_user: Option<AuthUser>,
) -> Result<Option<&'static str>, StatusCode> {
    let Some(def) = state.infra.registry.get_collection(collection_slug) else {
        return Ok(None);
    };
    let def = def.clone();

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
        return Ok(Some(CACHE_IMMUTABLE));
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

    let db_kind = state.infra.pool.kind().to_string();

    match task::spawn_blocking(move || upload_doc_visible(&input)).await {
        Ok(Ok(visible)) => Ok(visible.then_some(CACHE_PRIVATE)),
        Ok(Err(e)) => Err(db_error_status(e, &db_kind, "Upload access check")),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
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

    // Signed URL first: a valid capability serves without cookie/Bearer.
    // Anything less than valid falls through to the normal auth + gate.
    let signed_cache = check_signed_url(&state, &collection_slug, &filename, request.uri().query());

    let cache_control: String = if let Some(cache) = signed_cache {
        cache
    } else {
        // Token validation + user load is synchronous DB work — run
        // it on the blocking pool. (The access gate below is already async
        // / `spawn_blocking` internally.)
        let auth_user = match on_blocking_section(|| extract_auth_user(&request, &state)) {
            Ok(user) => user,
            Err(status) => return status.into_response(),
        };

        let cache = match check_upload_access(&state, &collection_slug, &filename, auth_user).await
        {
            Ok(Some(cache)) => cache,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(status) => return status.into_response(),
        };

        cache.to_string()
    };

    let serve = ServeRequest {
        collection_slug: &collection_slug,
        filename: &filename,
        cache_control: &cache_control,
        accepts_avif: accept.contains("image/avif"),
        accepts_webp: accept.contains("image/webp"),
    };

    serve_file(&state, &serve, request).await
}

/// Resolve the viewer of an upload through the shared auth evaluator (admin
/// surface), so the collection's accepted methods, locked accounts and stale
/// sessions decide exactly as they do for the admin UI. A credential that no
/// longer authenticates is served like an anonymous visitor: the access gate
/// decides, never the dead credential. A database error answers `503` or
/// `500`, never an anonymous read.
fn extract_auth_user(
    request: &Request<Body>,
    state: &AdminState,
) -> Result<Option<AuthUser>, StatusCode> {
    let headers = request.headers();

    let resolution = evaluate_admin_request(
        state,
        headers,
        bearer_token(headers),
        session_cookie_token(headers),
    )
    .map_err(|e| db_error_status(e, state.infra.pool.kind(), "Upload serve auth"))?;

    Ok(match resolution {
        Resolution::Authenticated(auth) => Some(auth.user),
        Resolution::Anonymous | Resolution::Invalid(_) => None,
    })
}

/// What one resolved serve request asks for: which file, under which cache
/// policy, and which negotiated image formats the viewer accepts.
struct ServeRequest<'a> {
    collection_slug: &'a str,
    filename: &'a str,
    cache_control: &'a str,
    accepts_avif: bool,
    accepts_webp: bool,
}

/// Whether a negotiated-variant response actually served the variant. A miss
/// or a backend failure falls through to the next candidate — and finally to
/// the original file — rather than failing the whole request.
fn variant_served(status: StatusCode) -> bool {
    !status.is_server_error() && status != StatusCode::NOT_FOUND
}

/// Serve the best negotiated variant of `req`, or `None` when none of them is
/// available on this backend.
async fn serve_variant(
    state: &AdminState,
    req: &ServeRequest<'_>,
    conditional: &ConditionalHeaders,
) -> Option<Response> {
    let storage = &state.infra.storage;

    for (variant_name, variant_mime) in
        negotiate_variants(req.filename, req.accepts_avif, req.accepts_webp)
    {
        let variant_key = format!("{}/{variant_name}", req.collection_slug);

        let headers = ServeHeaders::new(variant_mime, req.cache_control)
            .varied(true)
            .stored_name(Some(&variant_name));

        let Some(local_path) = storage.local_path(&variant_key) else {
            let response = serve_remote(storage, &variant_key, conditional, &headers).await;

            if variant_served(response.status()) {
                return Some(response);
            }

            continue;
        };

        if local_path.exists() {
            return Some(serve_local(&local_path, conditional, &headers).await);
        }
    }

    None
}

async fn serve_file(
    state: &AdminState,
    req: &ServeRequest<'_>,
    original_request: Request<Body>,
) -> Response {
    // Conditional / range headers are answered by `ServeFile` on the local
    // path and by the remote path itself; both need them off the original.
    let conditional = extract_conditional_headers(&original_request);

    if let Some(response) = serve_variant(state, req, &conditional).await {
        return response;
    }

    let storage = &state.infra.storage;
    let original_key = format!("{}/{}", req.collection_slug, req.filename);

    let requested_mime = mime_guess::from_path(req.filename)
        .first_or_octet_stream()
        .to_string();
    let is_image = requested_mime.starts_with("image/");

    let headers = ServeHeaders::new(&requested_mime, req.cache_control)
        .varied(is_image)
        .stored_name(Some(req.filename));

    let Some(local_path) = storage.local_path(&original_key) else {
        return serve_remote(storage, &original_key, &conditional, &headers).await;
    };

    if !local_path.exists() {
        return StatusCode::NOT_FOUND.into_response();
    }

    serve_local(&local_path, &conditional, &headers).await
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

/// Serve a local file via `tower_http::services::ServeFile`, which answers
/// Range, `ETag`, `Last-Modified` and conditional GETs itself, and then apply
/// the headers every served upload carries.
async fn serve_local(
    path: &path::Path,
    conditional: &ConditionalHeaders,
    headers: &ServeHeaders<'_>,
) -> Response {
    let service = ServeFile::new(path);

    let mut response = match service.oneshot(build_serve_request(conditional)).await {
        Ok(r) => r.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    headers.apply(&mut response);
    response
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::anyhow;
    use axum::http::header::{
        ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_RANGE, CONTENT_SECURITY_POLICY, VARY,
    };

    use super::*;

    /// A request that carries no range and no validators.
    fn no_conditions() -> ConditionalHeaders {
        ConditionalHeaders {
            range: None,
            if_none_match: None,
            if_modified_since: None,
        }
    }

    fn disposition_of(response: &Response) -> &str {
        response
            .headers()
            .get(CONTENT_DISPOSITION)
            .expect("a disposition is always set")
            .to_str()
            .expect("a valid header value")
    }

    /// Regression: a database error while resolving the owning document answered
    /// 404, telling a signed-in viewer under load that the file doesn't exist.
    #[test]
    fn a_transient_read_error_is_retryable_and_a_refusal_is_not_visible() {
        let transient: Result<(), ServiceError> = Err(ServiceError::Transient(anyhow!(
            "timed out waiting for connection"
        )));
        assert!(visible_or_retry(transient, |()| true).is_err());

        let refused: Result<(), ServiceError> = Err(ServiceError::AccessDenied("no".into()));
        assert!(!visible_or_retry(refused, |()| true).unwrap());

        assert!(visible_or_retry(Ok(()), |()| true).unwrap());
    }

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
    async fn serve_local_image_disposition_inline() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.png");
        fs::write(&path, b"fake png").unwrap();

        let headers = ServeHeaders::new("image/png", "public");
        let resp = serve_local(&path, &no_conditions(), &headers).await;

        assert_eq!(disposition_of(&resp), "inline");
    }

    #[tokio::test]
    async fn serve_local_pdf_disposition_attachment() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.pdf");
        fs::write(&path, b"fake pdf").unwrap();

        let headers =
            ServeHeaders::new("application/pdf", "public").stored_name(Some("abcdefghij_test.pdf"));
        let resp = serve_local(&path, &no_conditions(), &headers).await;

        assert_eq!(disposition_of(&resp), "attachment; filename=\"test.pdf\"");
    }

    #[tokio::test]
    async fn serve_local_varied_sets_vary() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.jpg");
        fs::write(&path, b"fake jpg").unwrap();

        let headers = ServeHeaders::new("image/jpeg", "public").varied(true);
        let resp = serve_local(&path, &no_conditions(), &headers).await;

        assert_eq!(resp.headers().get(VARY).unwrap(), "Accept");
    }

    #[tokio::test]
    async fn serve_local_no_vary_when_not_set() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, b"hello").unwrap();

        let resp = serve_local(
            &path,
            &no_conditions(),
            &ServeHeaders::new("text/plain", "no-cache"),
        )
        .await;

        // ServeFile may set Vary internally, but we don't set it
        assert!(!resp.headers().get_all(VARY).iter().any(|v| v == "Accept"));
    }

    #[tokio::test]
    async fn serve_local_svg_attachment_and_csp() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.svg");
        fs::write(&path, b"<svg></svg>").unwrap();

        let resp = serve_local(
            &path,
            &no_conditions(),
            &ServeHeaders::new("image/svg+xml", "public"),
        )
        .await;

        assert_eq!(disposition_of(&resp), "attachment");
        let csp = resp
            .headers()
            .get(CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(csp, "sandbox; default-src 'none'");
    }

    /// Every backend advertises ranged reads now, the local one included.
    #[tokio::test]
    async fn serve_local_advertises_range_support() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.bin");
        fs::write(&path, b"0123456789").unwrap();

        let resp = serve_local(
            &path,
            &no_conditions(),
            &ServeHeaders::new("application/octet-stream", "public"),
        )
        .await;

        assert_eq!(resp.headers().get(ACCEPT_RANGES).unwrap(), "bytes");
    }

    /// The local backend answers a range itself; the shared headers must not
    /// disturb the `206` it produced.
    #[tokio::test]
    async fn serve_local_answers_a_range_with_206() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.bin");
        fs::write(&path, b"0123456789").unwrap();

        let conditional = ConditionalHeaders {
            range: Some("bytes=2-4".parse().unwrap()),
            if_none_match: None,
            if_modified_since: None,
        };

        let resp = serve_local(
            &path,
            &conditional,
            &ServeHeaders::new("application/octet-stream", "public"),
        )
        .await;

        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(resp.headers().get(CONTENT_RANGE).unwrap(), "bytes 2-4/10");
    }

    /// A variant that is missing (404) or whose backend failed (503) must fall
    /// through to the next candidate, never end the request.
    #[test]
    fn only_a_real_variant_response_ends_negotiation() {
        assert!(variant_served(StatusCode::OK));
        assert!(variant_served(StatusCode::PARTIAL_CONTENT));
        assert!(variant_served(StatusCode::NOT_MODIFIED));
        assert!(variant_served(StatusCode::RANGE_NOT_SATISFIABLE));

        assert!(!variant_served(StatusCode::NOT_FOUND));
        assert!(!variant_served(StatusCode::SERVICE_UNAVAILABLE));
    }
}
