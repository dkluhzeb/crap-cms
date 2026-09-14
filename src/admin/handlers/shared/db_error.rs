//! The status for an error an admin handler can't recover from.

use axum::http::StatusCode;
use tracing::error;

use crate::service::ServiceError;

/// The status for an error an admin handler can't recover from — a pool
/// checkout, an auth lookup, a failing route — with database errors classified
/// as every surface classifies them: an exhausted or busy pool is `503`,
/// anything else `500`. The full error chain is logged under `context`, never
/// sent to the client.
pub(crate) fn db_error_status(e: anyhow::Error, db_kind: &str, context: &str) -> StatusCode {
    error!("{context}: {e:#}");

    match ServiceError::classify(e, db_kind) {
        ServiceError::Transient(_) => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    /// Regression: admin file serving and custom routes treated a database error
    /// during auth as an anonymous request, so a signed-in user was refused
    /// under load instead of told to retry.
    #[test]
    fn an_exhausted_pool_is_unavailable_and_anything_else_an_error() {
        let exhausted =
            anyhow!("timed out waiting for connection").context("Failed to get DB connection");

        assert_eq!(
            db_error_status(exhausted, "sqlite", "auth"),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            db_error_status(anyhow!("disk I/O error"), "sqlite", "auth"),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
