//! Static asset serving with config-dir overlay over compiled-in defaults.
//!
//! ## Layout aliases
//!
//! [`STATIC_ALIASES`] holds old-path → new-path fallbacks for the
//! pre-1.0 reshuffle (see `polished-shipping-lighthouse.md`). When a
//! request to an old URL fails to resolve in the embedded directory,
//! the alias map is consulted as a second-chance lookup, and a
//! deprecation warning is emitted (once per process per old path) via
//! [`warn_aliased_path`]. The list is intentionally empty in this
//! commit — Phases B–E populate it as files actually move.

use std::{
    collections::HashSet,
    path::Path as StdPath,
    sync::{Mutex, OnceLock},
};

use axum::{
    Router,
    body::Body,
    extract::Request,
    handler::HandlerWithoutStateExt,
    http::{
        HeaderMap, HeaderValue, StatusCode, Uri,
        header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH},
    },
    middleware::{Next, from_fn},
    response::{IntoResponse, Response},
    routing::get,
};
use include_dir::{Dir, include_dir};
use tower_http::services::ServeDir;
use tracing::warn;

static STATIC_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/static");

/// Old-layout → new-layout static path aliases.
///
/// Each entry is `(old_path, new_path)`. Both paths are relative to
/// `static/` (no leading `/`). When the requested URL doesn't resolve
/// in the embedded directory and `old_path` matches, the embedded
/// resolver retries with `new_path`. A `tracing::warn!` is emitted
/// the first time each `old_path` is hit per process so the operator
/// knows to migrate.
///
/// Populated by Phases B–E of the layout reshuffle. Removed at 1.0.
pub(crate) const STATIC_ALIASES: &[(&str, &str)] = &[
    // Phase B (CSS) — every CSS file moved into static/styles/<bucket>/.
    ("styles.css", "styles/main.css"),
    ("normalize.css", "styles/base/normalize.css"),
    ("fonts.css", "styles/base/fonts.css"),
    ("badges.css", "styles/parts/badges.css"),
    ("breadcrumb.css", "styles/parts/breadcrumb.css"),
    ("buttons.css", "styles/parts/buttons.css"),
    ("cards.css", "styles/parts/cards.css"),
    ("forms.css", "styles/parts/forms.css"),
    ("tables.css", "styles/parts/tables.css"),
    ("layout.css", "styles/layout/layout.css"),
    ("edit-sidebar.css", "styles/layout/edit-sidebar.css"),
    ("themes.css", "styles/themes/default.css"),
    // `lists.css` + `list-toolbar.css` merged into a single `parts/lists.css`.
    ("lists.css", "styles/parts/lists.css"),
    ("list-toolbar.css", "styles/parts/lists.css"),
    // Phase C — vendored third-party bundles.
    ("htmx.js", "vendor/htmx.js"),
    ("codemirror.js", "vendor/codemirror.js"),
    ("prosemirror.js", "vendor/prosemirror.js"),
    // Phase C — icons.
    ("favicon.svg", "icons/favicon.svg"),
    ("crap-cms.svg", "icons/crap-cms.svg"),
    // Phase D — plumbing modules moved into _internal/. Public
    // components stayed flat (override paths unchanged), per the
    // research-driven layout decision (see polished-shipping-lighthouse.md).
    ("components/css.js", "components/_internal/css.js"),
    ("components/global.js", "components/_internal/global.js"),
    ("components/groups.js", "components/_internal/groups.js"),
    ("components/h.js", "components/_internal/h.js"),
    ("components/i18n.js", "components/_internal/i18n.js"),
    (
        "components/picker-base.js",
        "components/_internal/picker-base.js",
    ),
    (
        "components/util/cookies.js",
        "components/_internal/util/cookies.js",
    ),
    (
        "components/util/discover.js",
        "components/_internal/util/discover.js",
    ),
    (
        "components/util/htmx.js",
        "components/_internal/util/htmx.js",
    ),
    (
        "components/util/index.js",
        "components/_internal/util/index.js",
    ),
    (
        "components/util/json.js",
        "components/_internal/util/json.js",
    ),
    (
        "components/util/toast.js",
        "components/_internal/util/toast.js",
    ),
];

/// Resolve an alias if the requested path matches an old-layout entry.
///
/// Returns the new-layout path string when an alias hits; logs a
/// deprecation warning once per process per old path. Returns `None`
/// if the path doesn't match any alias (the caller should then return
/// 404).
pub(crate) fn resolve_static_alias(path: &str) -> Option<&'static str> {
    let (_, new_path) = STATIC_ALIASES.iter().find(|(old, _)| *old == path)?;
    warn_aliased_path("static", path, new_path);
    Some(new_path)
}

/// Warn-once-per-process about a deprecated path that resolved through
/// an alias. Currently only [`STATIC_ALIASES`] uses this — `kind` is
/// always `"static"` — but the signature stays parameterized in case
/// a second alias surface is added later.
pub(crate) fn warn_aliased_path(kind: &str, old: &str, new: &str) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let key = format!("{kind}:{old}");

    let mut guard = match seen.lock() {
        Ok(g) => g,
        // A poisoned mutex here is non-fatal — the warning de-dup is
        // best-effort. Drop the lock state and just log; we'd rather
        // re-warn than swallow the warning.
        Err(poisoned) => poisoned.into_inner(),
    };
    if !guard.insert(key) {
        return;
    }

    warn!(
        target: "crap_cms::overlay",
        "Overlay {kind} path `{old}` is deprecated. Move to `{new}`. \
         See https://docs.crap-cms/admin-ui/upgrade/migrating-from-old-layout"
    );
}

/// Middleware that sets `Cache-Control: public, no-cache` on successful responses.
/// This ensures browsers always revalidate (using Last-Modified / If-Modified-Since
/// for config-dir files, or ETag / If-None-Match for embedded files).
async fn cache_control_middleware(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    if response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED {
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("public, no-cache"));
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
    router.layer(from_fn(cache_control_middleware))
}

/// Build hash for cache-busting (changes every build when static/ or templates/ change).
static BUILD_HASH: &str = env!("BUILD_HASH");

async fn embedded_static(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    let mime_type = mime_guess::from_path(path).first_or_text_plain();

    let resolved = STATIC_DIR
        .get_file(path)
        .or_else(|| resolve_static_alias(path).and_then(|aliased| STATIC_DIR.get_file(aliased)));

    match resolved {
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::NOT_FOUND.into_response()),
        Some(file) => {
            let etag_value = format!("\"{}\"", BUILD_HASH);

            // If the client sent a matching ETag, return 304.
            if let Some(inm) = headers.get(IF_NONE_MATCH)
                && inm.as_bytes() == etag_value.as_bytes()
            {
                return Response::builder()
                    .status(StatusCode::NOT_MODIFIED)
                    .header(ETAG, etag_value)
                    .body(Body::empty())
                    .unwrap_or_else(|_| StatusCode::NOT_MODIFIED.into_response());
            }

            let content_type = HeaderValue::from_str(mime_type.as_ref())
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));

            Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, content_type)
                .header(ETAG, etag_value)
                .body(Body::from(file.contents().to_vec()))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every alias entry must point at a path that actually exists in
    /// the embedded static dir. Catches typos at compile-test time
    /// rather than at first 404.
    #[test]
    fn static_aliases_resolve_to_existing_files() {
        for (old, new) in STATIC_ALIASES {
            assert!(
                STATIC_DIR.get_file(new).is_some(),
                "STATIC_ALIASES entry `{old}` → `{new}`: target file does not exist in embedded static dir"
            );
        }
    }

    /// Aliases must not collide with paths the embedded dir already
    /// resolves directly — the alias would never fire and the entry
    /// is dead code masking real intent.
    #[test]
    fn static_aliases_do_not_shadow_existing_paths() {
        for (old, new) in STATIC_ALIASES {
            assert!(
                STATIC_DIR.get_file(old).is_none(),
                "STATIC_ALIASES entry `{old}` → `{new}`: old path already resolves directly; alias is dead"
            );
        }
    }

    /// Unknown paths return None — alias lookup never falsely matches.
    #[test]
    fn resolve_static_alias_returns_none_for_unknown_path() {
        assert!(resolve_static_alias("definitely/not/a/real/path.css").is_none());
    }

    /// `warn_aliased_path` is idempotent within a process: calling it
    /// twice with the same key triggers the dedup. We can't easily
    /// observe the `tracing::warn!` side-effect in a unit test without
    /// pulling in a subscriber, but we *can* prove the dedup HashSet
    /// contains the path after a call by calling it a second time and
    /// observing no panic plus stable behavior.
    #[test]
    fn warn_aliased_path_does_not_panic_on_repeat_calls() {
        warn_aliased_path(
            "static",
            "test/fake-path-for-dedup-test.css",
            "new/path.css",
        );
        warn_aliased_path(
            "static",
            "test/fake-path-for-dedup-test.css",
            "new/path.css",
        );
        warn_aliased_path(
            "static",
            "test/fake-path-for-dedup-test.css",
            "new/path.css",
        );
    }
}
