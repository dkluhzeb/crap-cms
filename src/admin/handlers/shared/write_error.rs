//! The toast a failed admin form write shows.

use tracing::error;

use crate::service::ServiceError;

/// Decide the toast message for a write error that fell through the form's typed
/// `AccessDenied` / `Validation` arms. A Lua hook abort reaches the admin path as
/// `Internal` (the form handlers don't run `classify`), so re-classify to surface
/// the hook's own message. Genuine internals (DB, pool, bugs) are logged and get a
/// generic message — so we never leak internal error text *and* never silently
/// redirect. `operation` is only used for the log line. Shared by the collection
/// and global edit forms so both surface a hook's message the same way.
pub(in crate::admin::handlers) fn write_error_toast(
    operation: &str,
    err: ServiceError,
    db_kind: &str,
) -> String {
    let classified = err.reclassify(db_kind);

    if let ServiceError::HookError(msg) = classified {
        return msg;
    }

    error!("{operation} error: {classified}");
    "Something went wrong while saving. Please try again.".to_string()
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    /// A Lua hook abort arrives as `Internal("runtime error: …")`; the toast must
    /// surface the hook's own message (so e.g. "price may only increase" reaches
    /// the user), not a generic one.
    #[test]
    fn write_error_toast_surfaces_hook_message() {
        let err = ServiceError::Internal(anyhow!(
            "runtime error: hooks/guard.lua:3: price may only increase"
        ));
        let msg = write_error_toast("Create", err, "sqlite");
        assert!(
            msg.contains("price may only increase"),
            "hook message must reach the user, got: {msg}"
        );
    }

    /// A direct `HookError` (e.g. the gRPC-classified form) passes through.
    #[test]
    fn write_error_toast_passes_through_hook_error() {
        let msg = write_error_toast("Update", ServiceError::HookError("nope".into()), "sqlite");
        assert_eq!(msg, "nope");
    }

    /// A genuine internal error must NOT leak its text — generic message only.
    #[test]
    fn write_error_toast_hides_internal_detail() {
        let err = ServiceError::Internal(anyhow!("disk I/O failure at /var/lib/db.sqlite"));
        let msg = write_error_toast("Create", err, "sqlite");
        assert!(msg.contains("Something went wrong"), "got: {msg}");
        assert!(
            !msg.contains("disk I/O") && !msg.contains("db.sqlite"),
            "must not leak internal error text, got: {msg}"
        );
    }
}
