//! htmx navigation detection — decides partial-vs-full page rendering.
//!
//! Admin navigation links target `#main` (`hx-target="#main"`), so an
//! htmx-issued page GET carries `HX-Request: true` and `HX-Target: main`.
//! For those requests [`render_page`](super::render_page) renders only the
//! page's `<title>` + main content (the `htmx_partial` branch in
//! `layout/base.hbs`) instead of the whole document — the header, sidebar,
//! scripts, and web-component singletons stay untouched in the DOM.
//!
//! Everything else renders the full document: direct browser navigations
//! (no htmx headers), and htmx **history-restore** requests
//! (`HX-History-Restore-Request: true`, sent on a history-cache miss) —
//! htmx expects a complete page there to extract the history element from.
//!
//! Redirect-after-POST flows need no special handling: XHR/fetch follow
//! redirects transparently and re-send the request headers, so the followed
//! GET still carries `HX-Target: main` and renders partial.

use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, request::Parts},
};

/// Extracted htmx navigation intent for a page request. Obtain via the
/// axum extractor; pass to [`render_page`](super::render_page).
#[derive(Debug, Clone, Copy, Default)]
pub struct HxNav {
    /// Render only `<title>` + main content instead of the full document.
    pub partial: bool,
}

impl HxNav {
    /// A full-page render — for render paths that are never htmx
    /// navigation targets (auth pages outside the admin shell).
    #[must_use]
    pub fn full() -> Self {
        Self::default()
    }

    /// Derive the navigation intent from request headers — for handlers
    /// that already extract a [`HeaderMap`] (keeps their extractor list
    /// short); the axum extractor below delegates here.
    #[must_use]
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let is_htmx = headers.get("hx-request").is_some_and(|v| v == "true");
        let targets_main = headers.get("hx-target").is_some_and(|v| v == "main");
        let history_restore = headers
            .get("hx-history-restore-request")
            .is_some_and(|v| v == "true");

        Self {
            partial: is_htmx && targets_main && !history_restore,
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for HxNav {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::from_headers(&parts.headers))
    }
}

#[cfg(test)]
mod tests {
    use axum::{extract::FromRequestParts, http::Request};

    use super::HxNav;

    async fn extract(headers: &[(&str, &str)]) -> HxNav {
        let mut builder = Request::builder().uri("/admin");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let (mut parts, ()) = builder.body(()).unwrap().into_parts();
        HxNav::from_request_parts(&mut parts, &()).await.unwrap()
    }

    #[tokio::test]
    async fn plain_request_is_full() {
        assert!(!extract(&[]).await.partial);
    }

    #[tokio::test]
    async fn htmx_main_target_is_partial() {
        assert!(
            extract(&[("hx-request", "true"), ("hx-target", "main")])
                .await
                .partial
        );
    }

    #[tokio::test]
    async fn htmx_without_main_target_is_full() {
        assert!(!extract(&[("hx-request", "true")]).await.partial);
        assert!(
            !extract(&[("hx-request", "true"), ("hx-target", "other")])
                .await
                .partial
        );
    }

    /// History-cache misses must get the FULL document — htmx extracts the
    /// history element from it.
    #[tokio::test]
    async fn history_restore_is_full() {
        assert!(
            !extract(&[
                ("hx-request", "true"),
                ("hx-target", "main"),
                ("hx-history-restore-request", "true"),
            ])
            .await
            .partial
        );
    }
}
