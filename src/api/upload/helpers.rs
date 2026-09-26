//! Shared helpers for upload API handlers: auth, JSON responses, error classification.

use std::sync::Arc;

use axum::{
    http::{
        HeaderMap, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use serde::Serialize;
use tracing::{error, warn};

use crate::{
    admin::{AdminState, FormParseError, server::evaluate_admin_request},
    core::{AuthUser, CollectionDefinition, Document},
    service::{
        ServiceError,
        auth::{AuthFailure, Resolution},
    },
};

/// JSON body for an error response: `{ "error": "<message>" }`.
#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

/// JSON body for upload create/update success: `{ "document": <doc> }`.
#[derive(Serialize)]
pub struct DocumentBody<'a> {
    pub document: &'a Document,
}

/// JSON body for upload delete success: `{ "success": true }`.
#[derive(Serialize)]
pub struct SuccessBody {
    pub success: bool,
}

/// Extract Bearer token string from an Authorization header value.
pub fn extract_bearer_token(auth_header: &str) -> Option<&str> {
    auth_header
        .strip_prefix("Bearer ")
        .filter(|s| !s.is_empty())
}

/// The bearer token of an `Authorization` header, `None` when there is no
/// header, or a `401` for a header that isn't a bearer credential.
fn bearer_from_headers(headers: &HeaderMap) -> Result<Option<&str>, Box<Response>> {
    let Some(value) = headers.get(AUTHORIZATION) else {
        return Ok(None);
    };

    let value = value.to_str().map_err(|_| {
        Box::new(json_error(
            StatusCode::UNAUTHORIZED,
            "Invalid Authorization header",
        ))
    })?;

    extract_bearer_token(value).map(Some).ok_or_else(|| {
        Box::new(json_error(
            StatusCode::UNAUTHORIZED,
            "Authorization header must use Bearer scheme",
        ))
    })
}

/// The response for a credential that was supplied but can't be used.
fn auth_failure_response(failure: AuthFailure) -> Response {
    let message = match failure {
        AuthFailure::Lookup => {
            return json_error(StatusCode::SERVICE_UNAVAILABLE, "User lookup failed");
        }
        AuthFailure::Locked => "Account locked",
        AuthFailure::StaleSession => "Session invalidated",
        AuthFailure::UserMissing => "User no longer exists",
        AuthFailure::UnknownCollection => "Auth collection no longer exists",
        AuthFailure::BadToken => "Invalid or expired token",
        AuthFailure::Unaccepted => "Credential not accepted on this surface",
        AuthFailure::MfaRequired => "Second factor required on this surface",
    };

    json_error(StatusCode::UNAUTHORIZED, message)
}

/// Resolve the caller of an upload API request through the shared auth
/// evaluator (admin surface): the collection's accepted methods, locked
/// accounts and stale sessions decide exactly as they do for the admin UI.
///
/// Returns `Ok(None)` for an anonymous request, `Ok(Some(user))` when a method
/// authenticated it, and an error response when a credential was supplied but
/// can't be used — never an anonymous fallback for a bad credential.
#[cfg(not(tarpaulin_include))]
pub fn extract_bearer_user(
    state: &AdminState,
    headers: &HeaderMap,
) -> Result<Option<AuthUser>, Box<Response>> {
    let bearer = bearer_from_headers(headers)?;

    // `db_error_response` logs the error it classifies.
    let resolution = evaluate_admin_request(state, headers, bearer, None)
        .map_err(|e| Box::new(db_error_response(e, state.infra.pool.kind())))?;

    match resolution {
        Resolution::Authenticated(auth) => Ok(Some(auth.user)),
        Resolution::Anonymous => Ok(None),
        Resolution::Invalid(failure) => Err(Box::new(auth_failure_response(failure))),
    }
}

/// The response to a multipart body that could not be read: `413` when it
/// exceeded the request size limit (an upload over the configured maximum),
/// `400` for anything else.
pub(super) fn multipart_error_response(err: &FormParseError) -> Response {
    error!("Upload multipart parse failed: {err}");

    if err.is_too_large() {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Request body exceeds the upload size limit",
        );
    }

    json_error(StatusCode::BAD_REQUEST, "Invalid multipart request")
}

/// Return a JSON error response.
pub fn json_error(status: StatusCode, message: &str) -> Response {
    json_ok(status, &ErrorBody { error: message })
}

/// Return a JSON success response with the given status and body.
pub fn json_ok<T: Serialize>(status: StatusCode, body: &T) -> Response {
    let serialized = serde_json::to_string(body).expect("response body serialize");

    (
        status,
        [(CONTENT_TYPE, "application/json; charset=utf-8")],
        serialized,
    )
        .into_response()
}

/// The caller of an upload API request and the upload collection it names, or
/// the response refusing it: an unusable credential, `404` for an unknown
/// collection, `400` for a collection without uploads.
///
/// The collection's access rule is not judged here. The service judges it —
/// for a write, on the request's data together with the file's own columns
/// and before the file is stored — and a rule judged here without that data
/// could refuse a request the rule allows.
#[cfg(not(tarpaulin_include))]
pub(super) fn resolve_upload_request(
    state: &AdminState,
    headers: &HeaderMap,
    slug: &str,
) -> Result<(Option<AuthUser>, Arc<CollectionDefinition>), Box<Response>> {
    let auth_user = extract_bearer_user(state, headers)?;

    let def = state
        .infra
        .registry
        .get_collection(slug)
        .cloned()
        .ok_or_else(|| {
            Box::new(json_error(
                StatusCode::NOT_FOUND,
                &format!("Collection '{slug}' not found"),
            ))
        })?;

    if !def.is_upload_collection() {
        return Err(Box::new(json_error(
            StatusCode::BAD_REQUEST,
            &format!("Collection '{slug}' is not an upload collection"),
        )));
    }

    Ok((auth_user, def))
}

/// The response to a failed connection checkout or transaction start: an
/// exhausted or busy pool is `503`, as every other surface reports it, and
/// anything else a generic `500`.
fn db_error_response(e: anyhow::Error, db_kind: &str) -> Response {
    service_error_to_response(&ServiceError::classify(e, db_kind))
}

/// Map a [`ServiceError`] to the appropriate JSON error response.
///
/// Semantic errors (validation, access, hook, not-found, unique violation,
/// referenced, auth failures) surface their `Display` to the client — the
/// text is user-facing and kept sanitized by the service layer.
///
/// `Transient` and `Internal` wrap raw backend / pool errors whose `Display`
/// can leak DB identifiers or driver vocabulary. Those are logged — a
/// transient (retryable, expected under load) at `warn`, an internal at
/// `error` — and the client receives a generic phrase only.
pub fn service_error_to_response(err: &ServiceError) -> Response {
    let (status, message) = match err {
        ServiceError::AccessDenied(_) => (StatusCode::FORBIDDEN, err.to_string()),
        ServiceError::NotFound(_) => (StatusCode::NOT_FOUND, err.to_string()),
        ServiceError::Validation(_) | ServiceError::HookError(_) => {
            (StatusCode::BAD_REQUEST, err.to_string())
        }
        // Same "refused due to data state" class as Referenced → 409.
        ServiceError::UniqueViolation(_)
        | ServiceError::ForeignKeyViolation(_)
        | ServiceError::Referenced { .. }
        | ServiceError::LimitExceeded(_)
        | ServiceError::Conflict(_) => (StatusCode::CONFLICT, err.to_string()),
        ServiceError::AccountLocked
        | ServiceError::EmailNotVerified
        | ServiceError::InvalidCredentials
        | ServiceError::InvalidToken { .. } => (StatusCode::UNAUTHORIZED, err.to_string()),
        ServiceError::Transient(_) => {
            warn!("Upload service transient error: {}", err);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Service temporarily unavailable".to_string(),
            )
        }
        ServiceError::Internal(_) => {
            error!("Upload service internal error: {}", err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal server error".to_string(),
            )
        }
    };

    json_error(status, &message)
}

/// Fixtures shared by the upload handlers' tests.
#[cfg(test)]
pub(super) mod test_support {
    use std::sync::Arc;

    use tempfile::TempDir;

    use crate::{
        admin::test_support::test_infra_with_events,
        core::{
            CollectionDefinition, EventReceiver, FieldDefinition, FieldType,
            upload::CollectionUpload,
        },
        service::AppInfra,
    };

    /// A populate-cache entry every fixture starts with, for a write to clear.
    pub const CACHED_KEY: &str = "populate:media:m1";

    /// The `media` upload collection with the metadata columns a non-image
    /// upload writes.
    fn media() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("media");
        def.upload = Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        });
        def.fields = vec![
            FieldDefinition::builder("filename", FieldType::Text).build(),
            FieldDefinition::builder("mime_type", FieldType::Text).build(),
            FieldDefinition::builder("filesize", FieldType::Number).build(),
            FieldDefinition::builder("url", FieldType::Text).build(),
        ];

        def
    }

    /// A full infra over the `media` collection with an in-process event bus,
    /// a receiver subscribed to it, and [`CACHED_KEY`] in the populate cache.
    pub fn infra_with_events() -> (TempDir, Arc<AppInfra>, EventReceiver) {
        let (tmp, infra, rx) = test_infra_with_events(media());

        infra.cache.set(CACHED_KEY, b"stale").unwrap();

        (tmp, infra, rx)
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use axum::body::to_bytes;

    use super::*;

    #[tokio::test]
    async fn json_error_returns_correct_status() {
        let resp = json_error(StatusCode::BAD_REQUEST, "something broke");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn json_error_body_contains_message() {
        let resp = json_error(StatusCode::NOT_FOUND, "not here");
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["error"], "not here");
    }

    #[tokio::test]
    async fn json_error_content_type() {
        let resp = json_error(StatusCode::INTERNAL_SERVER_ERROR, "oops");
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json; charset=utf-8"
        );
    }

    #[tokio::test]
    async fn json_ok_returns_correct_status() {
        let resp = json_ok(StatusCode::CREATED, &SuccessBody { success: true });
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn json_ok_body_matches() {
        let doc = Document::new("abc");
        let resp = json_ok(StatusCode::OK, &DocumentBody { document: &doc });
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["document"]["id"], "abc");
    }

    #[test]
    fn bearer_token_valid() {
        assert_eq!(extract_bearer_token("Bearer abc123"), Some("abc123"));
    }

    #[test]
    fn bearer_token_wrong_prefix() {
        assert_eq!(extract_bearer_token("Basic abc123"), None);
    }

    #[test]
    fn bearer_token_empty_value() {
        assert_eq!(extract_bearer_token("Bearer "), None);
    }

    #[test]
    fn bearer_token_lowercase() {
        assert_eq!(extract_bearer_token("bearer abc123"), None);
    }

    #[test]
    fn bearer_token_no_space() {
        assert_eq!(extract_bearer_token("Bearerabc123"), None);
    }

    // ── service_error_to_response (the one upload error mapper) ─────

    #[tokio::test]
    async fn service_error_access_denied_returns_403() {
        let resp = service_error_to_response(&ServiceError::AccessDenied("nope".into()));
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn service_error_not_found_returns_404() {
        let resp = service_error_to_response(&ServiceError::NotFound("gone".into()));
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn service_error_validation_returns_400() {
        use crate::core::validate::{FieldError, ValidationError};
        let ve = ValidationError::new(vec![FieldError::new("title", "required")]);
        let resp = service_error_to_response(&ServiceError::Validation(ve));
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn service_error_hook_error_returns_400() {
        let resp = service_error_to_response(&ServiceError::HookError("bad hook".into()));
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn service_error_unique_violation_returns_409() {
        let resp = service_error_to_response(&ServiceError::UniqueViolation("email".into()));
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn service_error_referenced_returns_409() {
        let resp = service_error_to_response(&ServiceError::Referenced {
            id: "doc-1".into(),
            count: 3,
        });
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn service_error_internal_returns_500_generic_message() {
        let resp = service_error_to_response(&ServiceError::Internal(anyhow!("secret details")));
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["error"], "Internal server error");
    }

    #[tokio::test]
    async fn service_error_transient_returns_503_generic_message() {
        // The raw DB error text ("database is locked", connection-pool errors,
        // driver identifiers) must not reach the client — logged only.
        let resp = service_error_to_response(&ServiceError::Transient(anyhow!(
            "database is locked (secret backend detail)"
        )));
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["error"], "Service temporarily unavailable");
        assert!(
            !parsed["error"].as_str().unwrap().contains("database"),
            "client must not see backend detail",
        );
    }

    /// Regression: the upload access check answered `500` when the pool was
    /// exhausted, where every other surface answers `503`.
    #[test]
    fn an_exhausted_pool_answers_503() {
        let err =
            anyhow!("timed out waiting for connection").context("Failed to get DB connection");

        assert_eq!(
            db_error_response(err, "sqlite").status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            db_error_response(anyhow!("disk I/O error"), "sqlite").status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn service_error_account_locked_returns_401() {
        let resp = service_error_to_response(&ServiceError::AccountLocked);
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn service_error_invalid_token_returns_401() {
        let resp = service_error_to_response(&ServiceError::InvalidToken {
            kind: "reset",
            reason: "expired",
        });
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
