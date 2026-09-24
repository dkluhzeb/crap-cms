//! The response to a create/update submission whose body could not be read.

use std::collections::HashMap;

use axum::{Extension, http::StatusCode, response::Response};
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::{forms::FormParseError, shared::error_toast},
    },
    core::{AuthUser, CollectionDefinition, upload::format_filesize},
};

/// Translation key of the toast for a body over the upload size limit.
const TOO_LARGE_KEY: &str = "upload_too_large";

/// Translation key of the toast for any other unreadable body.
const UNREADABLE_KEY: &str = "form_unreadable";

/// Answer a submission `parse_form` refused with an error toast and no body.
///
/// An error status is not swapped by htmx, so the form keeps every edit the
/// user made: `413` naming the size limit when the body was too large (an
/// upload over the maximum), `422` for any other unreadable body. A redirect
/// here would reload the page and silently throw the edits away.
pub(in crate::admin::handlers::collections) fn form_parse_error_response(
    state: &AdminState,
    def: &CollectionDefinition,
    auth_user: Option<&Extension<AuthUser>>,
    err: &FormParseError,
) -> Response {
    error!("Form parse failed for '{}': {err}", def.slug);

    let locale = auth_user.map_or("en", |Extension(au)| au.ui_locale.as_str());

    if !err.is_too_large() {
        let message = state.translations.get(locale, UNREADABLE_KEY);

        return error_toast(StatusCode::UNPROCESSABLE_ENTITY, message);
    }

    let params = HashMap::from([(
        "max".to_string(),
        format_filesize(def.max_upload_size(state.config.upload.max_file_size)),
    )]);
    let message = state
        .translations
        .get_interpolated(locale, TOO_LARGE_KEY, &params);

    error_toast(StatusCode::PAYLOAD_TOO_LARGE, &message)
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;

    use super::*;
    use crate::{admin::test_state::test_admin_state, core::upload::CollectionUpload};

    fn toast_of(resp: &Response) -> String {
        resp.headers()
            .get("X-Crap-Toast")
            .expect("an error toast")
            .to_str()
            .unwrap()
            .to_string()
    }

    fn media_def(max: Option<u64>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("media");
        let mut upload = CollectionUpload::new();
        upload.max_file_size = max;
        def.upload = Some(upload);
        def
    }

    /// Regression: an upload over the body limit answered a plain redirect,
    /// which htmx followed and swapped in a fresh form — the edits were lost
    /// without a word.
    #[tokio::test]
    async fn an_oversized_body_answers_413_with_a_toast_and_no_body() {
        let state = test_admin_state();
        let err = FormParseError::new(StatusCode::PAYLOAD_TOO_LARGE, "limit".into());

        let resp = form_parse_error_response(&state, &media_def(Some(2048)), None, &err);

        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(resp.headers().get("location").is_none(), "no redirect");

        let toast = toast_of(&resp);
        assert!(toast.contains("2.0 KB"), "names the limit: {toast}");

        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert!(body.is_empty(), "nothing for htmx to swap");
    }

    #[test]
    fn an_unreadable_body_answers_422_with_a_toast() {
        let state = test_admin_state();
        let err = FormParseError::new(StatusCode::BAD_REQUEST, "bad".into());

        let resp = form_parse_error_response(&state, &media_def(None), None, &err);

        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(toast_of(&resp).contains("could not be read"));
    }

    #[test]
    fn the_limit_falls_back_to_the_global_maximum() {
        let state = test_admin_state();

        let global = state.config.upload.max_file_size;

        assert_eq!(media_def(None).max_upload_size(global), global);
        assert_eq!(media_def(Some(7)).max_upload_size(global), 7);
    }
}
