//! Request body-size limits.
//!
//! Every route buffers at most the global upload maximum plus form headroom
//! ([`global_body_limit`]). Only the routes that take a file — an upload
//! collection's admin create/update and the `/api/upload` routes — raise that
//! to the target collection's own upload maximum ([`upload_body_limit`]), so a
//! collection allowing large files never lifts the cap on the login form, the
//! JSON endpoints, or MCP. The public, pre-authentication routes go the other
//! way: their small forms get `[server] auth_body_limit` ([`auth_body_limit`]).

use axum::{
    extract::{DefaultBodyLimit, RawPathParams, Request, State},
    middleware::Next,
    response::Response,
};
use tower::{Layer, ServiceExt};

use crate::admin::AdminState;

/// Room for the non-file form fields next to the largest accepted file.
const FORM_HEADROOM: u64 = 1024 * 1024;

/// The limit used when the computed one does not fit in `usize`.
const FALLBACK_LIMIT: usize = 50 * 1024 * 1024;

/// The path parameter naming the target collection on upload routes.
const SLUG_PARAM: &str = "slug";

/// The collection an upload route targets: its `{slug}` path parameter.
pub(crate) fn target_slug(params: &RawPathParams) -> Option<&str> {
    params
        .iter()
        .find_map(|(name, value)| (name == SLUG_PARAM).then_some(value))
}

/// The body limit for a request carrying a file of at most `max_upload` bytes.
fn body_limit_for(max_upload: u64) -> usize {
    usize::try_from(max_upload.saturating_add(FORM_HEADROOM)).unwrap_or(FALLBACK_LIMIT)
}

/// The body limit every route gets: the global upload maximum plus headroom.
pub(crate) fn global_body_limit(state: &AdminState) -> usize {
    body_limit_for(state.config.upload.max_file_size)
}

/// The body limit of the public, pre-authentication routes (login, password
/// reset, MFA, auth callbacks): `[server] auth_body_limit`, independent of any
/// upload maximum.
pub(crate) fn auth_body_limit(state: &AdminState) -> usize {
    usize::try_from(state.config.server.auth_body_limit).unwrap_or(usize::MAX)
}

/// The body limit for a file upload into collection `slug`: its own upload
/// maximum plus headroom, or the global limit for an unknown collection or
/// one without uploads.
fn collection_body_limit(state: &AdminState, slug: Option<&str>) -> usize {
    let global = state.config.upload.max_file_size;

    let max_upload = slug
        .and_then(|slug| state.infra.registry.get_collection(slug))
        .map_or(global, |def| def.max_upload_size(global));

    body_limit_for(max_upload)
}

/// Route middleware for the routes that accept a file: replace the global body
/// limit with the target collection's (named by the `{slug}` path parameter).
/// The limit is applied through axum's own [`DefaultBodyLimit`], so an
/// oversized body is refused by the extractors exactly as before — `413`.
pub(crate) async fn upload_body_limit(
    State(state): State<AdminState>,
    params: RawPathParams,
    request: Request,
    next: Next,
) -> Response {
    let limit = collection_body_limit(&state, target_slug(&params));

    DefaultBodyLimit::max(limit)
        .layer(next)
        .oneshot(request)
        .await
        .unwrap_or_else(|never| match never {})
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::{
        admin::test_state::test_admin_state_with_registry,
        core::{CollectionDefinition, Registry, upload::CollectionUpload},
    };

    const MIB: u64 = 1024 * 1024;

    fn limit(bytes: u64) -> usize {
        usize::try_from(bytes).unwrap()
    }

    #[test]
    fn the_limit_is_the_upload_maximum_plus_form_headroom() {
        assert_eq!(body_limit_for(10 * MIB), limit(11 * MIB));

        // Saturates instead of overflowing.
        assert!(body_limit_for(u64::MAX) >= FALLBACK_LIMIT);
    }

    /// Regression: the pre-auth routes inherited the upload-sized global limit,
    /// so an anonymous login POST could make the server buffer tens of
    /// megabytes. Their limit follows `auth_body_limit` alone.
    #[test]
    fn the_auth_limit_ignores_the_upload_maximum() {
        let mut state = test_admin_state_with_registry(Registry::default());
        state.config.upload.max_file_size = 500 * MIB;
        state.config.server.auth_body_limit = 64 * 1024;

        assert_eq!(auth_body_limit(&state), 64 * 1024);
        assert!(global_body_limit(&state) > auth_body_limit(&state));
    }

    /// Regression: the largest collection maximum used to become the limit of
    /// every route. The global limit follows the global maximum alone; only an
    /// upload into the collection gets the collection's.
    #[test]
    fn only_an_upload_into_the_collection_gets_its_larger_limit() {
        let mut media = CollectionDefinition::new("media");
        let mut upload = CollectionUpload::new();
        upload.max_file_size = Some(40 * MIB);
        media.upload = Some(upload);

        let mut registry = Registry::default();
        registry.register_collection(media);

        let mut state = test_admin_state_with_registry(registry);
        state.config.upload.max_file_size = 10 * MIB;

        assert_eq!(global_body_limit(&state), limit(11 * MIB));
        assert_eq!(
            collection_body_limit(&state, Some("media")),
            limit(41 * MIB)
        );
        assert_eq!(
            collection_body_limit(&state, Some("unknown")),
            limit(11 * MIB)
        );
        assert_eq!(collection_body_limit(&state, None), limit(11 * MIB));
    }
}
