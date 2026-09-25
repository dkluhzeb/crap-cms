//! The response a failed admin form write gets.

use axum::{http::StatusCode, response::Response};
use tracing::{error, warn};

use crate::{
    admin::{AdminState, handlers::shared::error_toast},
    core::upload::ImageProcessingBusy,
    service::ServiceError,
};

/// Translation key of the toast for a write that failed for an internal reason.
const SAVE_FAILED_KEY: &str = "error_save_failed";

/// Translation key of the toast for a write refused while the server was busy.
const BUSY_KEY: &str = "error_busy";

/// Translation key of the toast for an upload refused while every
/// image-processing slot was taken.
const IMAGES_BUSY_KEY: &str = "error_busy_images";

/// What the toast of a failed write says.
#[derive(Debug, PartialEq, Eq)]
enum WriteFailure {
    /// A hook's own message, shown as it is.
    Hook(String),
    /// A resource was at capacity (the database, the connection pool, image
    /// processing): nothing is wrong with the write, and it succeeds when
    /// retried in a moment.
    Busy { images: bool },
    /// Anything else — logged, never shown.
    Failed,
}

impl WriteFailure {
    /// The status and translation key of the toast.
    fn status_and_key(&self) -> (StatusCode, &'static str) {
        match self {
            Self::Busy { images: true } => (StatusCode::SERVICE_UNAVAILABLE, IMAGES_BUSY_KEY),
            Self::Busy { images: false } => (StatusCode::SERVICE_UNAVAILABLE, BUSY_KEY),
            Self::Hook(_) | Self::Failed => (StatusCode::UNPROCESSABLE_ENTITY, SAVE_FAILED_KEY),
        }
    }
}

/// Classify a write error that fell through the form's typed `AccessDenied` /
/// `Validation` arms, and log it at the level it deserves.
///
/// A Lua hook abort reaches the admin path as `Internal` (the form handlers
/// don't run `classify`), so it is re-classified to surface the hook's own
/// message. A capacity error is expected under load and logged as a warning;
/// a genuine internal (a bug, a broken database) is logged as an error and its
/// text is never shown. `operation` is only used for the log line.
fn classify_write_failure(operation: &str, err: ServiceError, db_kind: &str) -> WriteFailure {
    match err.reclassify(db_kind) {
        ServiceError::HookError(msg) => WriteFailure::Hook(msg),
        ServiceError::Transient(e) => {
            warn!("{operation} refused while busy: {e:#}");

            WriteFailure::Busy {
                images: e.is::<ImageProcessingBusy>(),
            }
        }
        other => {
            error!("{operation} error: {other}");

            WriteFailure::Failed
        }
    }
}

/// The error toast for a failed collection or global form write — so both edit
/// forms surface a hook's message, a busy server and an internal failure the
/// same way: a hook's message as it is, a busy server as `503` asking to try
/// again, anything else as a generic translated message. An error status is
/// never swapped by htmx, so the form keeps the user's edits.
pub(in crate::admin::handlers) fn write_error_response(
    state: &AdminState,
    ui_locale: &str,
    operation: &str,
    err: ServiceError,
) -> Response {
    let failure = classify_write_failure(operation, err, state.infra.pool.kind());
    let (status, key) = failure.status_and_key();

    if let WriteFailure::Hook(msg) = failure {
        return error_toast(status, &msg);
    }

    error_toast(status, state.translations.get(ui_locale, key))
}

#[cfg(test)]
mod tests {
    use anyhow::{Error, anyhow};

    use super::*;
    use crate::admin::test_state::test_admin_state;

    fn toast_of(resp: &Response) -> String {
        resp.headers()
            .get("X-Crap-Toast")
            .expect("an error toast")
            .to_str()
            .unwrap()
            .to_string()
    }

    /// A Lua hook abort arrives as `Internal("runtime error: …")`; the toast must
    /// surface the hook's own message (so e.g. "price may only increase" reaches
    /// the user), not a generic one.
    #[test]
    fn a_hook_abort_surfaces_its_message() {
        let err = ServiceError::Internal(anyhow!(
            "runtime error: hooks/guard.lua:3: price may only increase"
        ));

        let resp = write_error_response(&test_admin_state(), "en", "Create", err);

        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(toast_of(&resp).contains("price may only increase"));
    }

    /// A direct `HookError` (e.g. the gRPC-classified form) passes through.
    #[test]
    fn a_hook_error_passes_through() {
        assert_eq!(
            classify_write_failure("Update", ServiceError::HookError("nope".into()), "sqlite"),
            WriteFailure::Hook("nope".into())
        );
    }

    /// A genuine internal error must NOT leak its text — generic message only.
    #[test]
    fn an_internal_error_hides_its_detail() {
        let err = ServiceError::Internal(anyhow!("disk I/O failure at /var/lib/db.sqlite"));

        let resp = write_error_response(&test_admin_state(), "en", "Create", err);
        let toast = toast_of(&resp);

        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(toast.contains("Something went wrong"), "got: {toast}");
        assert!(!toast.contains("disk I/O") && !toast.contains("db.sqlite"));
    }

    /// Regression: an upload refused because every image-processing slot was
    /// taken showed the generic "Something went wrong" toast with a 422 and
    /// was logged as an error — though the same upload succeeds in a moment.
    #[test]
    fn a_busy_upload_asks_to_try_again_with_503() {
        let err = ServiceError::Transient(Error::new(ImageProcessingBusy));

        let resp = write_error_response(&test_admin_state(), "en", "Create", err);
        let toast = toast_of(&resp);

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(toast.contains("busy processing images"), "got: {toast}");
    }

    /// Any other capacity error — a locked database, an exhausted pool — is
    /// just as retryable, and says so.
    #[test]
    fn a_busy_database_asks_to_try_again_with_503() {
        let err = ServiceError::Internal(anyhow!("database is locked"));

        let resp = write_error_response(&test_admin_state(), "en", "Update", err);
        let toast = toast_of(&resp);

        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(toast.contains("busy"), "got: {toast}");
        assert!(!toast.contains("images"), "got: {toast}");
    }

    /// The toasts are translated into the user's UI locale.
    #[test]
    fn the_busy_toast_is_translated() {
        let err = ServiceError::Transient(Error::new(ImageProcessingBusy));

        let resp = write_error_response(&test_admin_state(), "de", "Create", err);

        assert!(
            !toast_of(&resp).contains("busy processing images"),
            "the German toast must not be the English one"
        );
    }
}
