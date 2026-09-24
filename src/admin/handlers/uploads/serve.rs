//! Serves uploaded files with access-control-aware caching.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, Request, StatusCode, header::ACCEPT},
    response::{IntoResponse, Response},
};
use tokio::task;

use crate::{
    admin::{
        AdminState,
        handlers::{
            shared::{db_error_status, response::on_blocking_section},
            uploads::{
                delivery::{ServeRequest, serve_file},
                visibility::{UploadVisibilityInput, upload_doc_visible},
            },
        },
        server::{bearer_token, evaluate_admin_request, session_cookie_token},
    },
    core::{
        AuthUser, CollectionDefinition,
        upload::{served_url, verify_upload_sig},
    },
    hooks::HookEvent,
    service::auth::Resolution,
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

/// Whether every file of `def` is unconditionally public, so it can be served
/// CDN-cacheable without a query.
///
/// Only when "no read hook" genuinely means ALLOW — `default_deny` is off —
/// there is no draft/trash axis (no status- or viewer-dependent visibility),
/// and no `before_read` hook, the collection's own or a registered global one,
/// could abort the read: a read the gated path would refuse must never be
/// served from the fast path. Under `default_deny` (the secure default) a
/// hook-less collection denies reads, so it falls through to the
/// access-resolving path, which 404s correctly.
fn provably_public(
    default_deny: bool,
    def: &CollectionDefinition,
    global_before_read: bool,
) -> bool {
    !default_deny
        && def.access.read.is_none()
        && !def.has_drafts()
        && !def.soft_delete
        && def.hooks.before_read.is_empty()
        && !global_before_read
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

    let global_before_read = state
        .infra
        .hook_runner
        .has_registered_hooks_for(HookEvent::BeforeRead.as_str());

    if provably_public(state.config.access.default_deny, &def, global_before_read) {
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

/// Resolve the viewer of an upload through the shared auth evaluator (admin
/// surface), so the collection's accepted methods, locked accounts and stale
/// sessions decide exactly as they do for the admin UI. A credential that no
/// longer authenticates is served like an anonymous visitor: the access gate
/// decides, never the dead credential. A database error answers `503` or
/// `500`, never an anonymous read.
fn extract_auth_user(
    headers: &HeaderMap,
    state: &AdminState,
) -> Result<Option<AuthUser>, StatusCode> {
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

/// The file one serve request names.
struct ServeTarget<'a> {
    collection_slug: &'a str,
    filename: &'a str,
}

impl<'a> ServeTarget<'a> {
    fn new(collection_slug: &'a str, filename: &'a str) -> Self {
        Self {
            collection_slug,
            filename,
        }
    }
}

/// The cache policy to serve the requested file under, or the status that
/// refuses it: a valid signed URL first (it serves without cookie/Bearer), and
/// anything less than valid falls through to the viewer's own auth + gate.
async fn resolve_cache_control(
    state: &AdminState,
    target: &ServeTarget<'_>,
    headers: &HeaderMap,
    query: Option<&str>,
) -> Result<String, StatusCode> {
    if let Some(cache) = check_signed_url(state, target.collection_slug, target.filename, query) {
        return Ok(cache);
    }

    // Token validation + user load is synchronous DB work — run it on the
    // blocking pool. (The access gate below is already async /
    // `spawn_blocking` internally.)
    let auth_user = on_blocking_section(|| extract_auth_user(headers, state))?;

    check_upload_access(state, target.collection_slug, target.filename, auth_user)
        .await?
        .map(str::to_string)
        .ok_or(StatusCode::NOT_FOUND)
}

/// Serve an uploaded file, checking collection read access if configured.
///
/// Supports content negotiation for generated image sizes: if the browser
/// Accept header includes `image/avif` or `image/webp`, and the collection
/// writes that format variant, the more efficient format is served instead.
pub async fn serve_upload(
    State(state): State<AdminState>,
    Path((collection_slug, filename)): Path<(String, String)>,
    request: Request<Body>,
) -> Response {
    if has_path_traversal(&collection_slug) || has_path_traversal(&filename) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let target = ServeTarget::new(&collection_slug, &filename);
    let query = request.uri().query();

    let cache_control = match resolve_cache_control(&state, &target, request.headers(), query).await
    {
        Ok(cache) => cache,
        Err(status) => return status.into_response(),
    };

    let accept = request
        .headers()
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let def = state.infra.registry.get_collection(&collection_slug);

    let serve = ServeRequest {
        collection_slug: &collection_slug,
        filename: &filename,
        cache_control: &cache_control,
        upload: def.and_then(|d| d.upload.as_ref()).filter(|u| u.enabled),
        accepts_avif: accept.contains("image/avif"),
        accepts_webp: accept.contains("image/webp"),
    };

    serve_file(&state, &serve, request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::HookRef;

    /// Regression: the public fast path ignored `before_read` hooks, so a file
    /// whose read a hook aborts on every other surface was served — and cached
    /// immutable by every CDN in between.
    #[test]
    fn a_before_read_hook_disables_the_public_fast_path() {
        let def = CollectionDefinition::new("media");
        assert!(provably_public(false, &def, false));

        assert!(
            !provably_public(false, &def, true),
            "a registered global before_read hook can abort the read"
        );

        let mut hooked = def;
        hooked.hooks.before_read = vec![HookRef::new("hooks.gate")];
        assert!(
            !provably_public(false, &hooked, false),
            "the collection's own before_read hook can abort the read"
        );
    }

    /// Every other axis the fast path must not skip.
    #[test]
    fn the_public_fast_path_needs_no_access_axis_at_all() {
        let def = CollectionDefinition::new("media");
        assert!(!provably_public(true, &def, false), "default_deny");

        let mut read_hook = def.clone();
        read_hook.access.read = Some(HookRef::new("hooks.read"));
        assert!(!provably_public(false, &read_hook, false), "access.read");

        let mut trash = def;
        trash.soft_delete = true;
        assert!(!provably_public(false, &trash, false), "soft delete");
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
}
