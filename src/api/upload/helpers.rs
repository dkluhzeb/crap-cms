//! Shared helpers for upload API handlers: auth, JSON responses, error classification.

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
    admin::{AdminState, server::evaluate_admin_request},
    core::{
        AuthUser, CollectionDefinition, Document, DocumentFields, HookRef,
        collection::LiveMode,
        event::{EventOperation, EventTarget, EventUser, EventViewMeta},
    },
    db::AccessResult,
    hooks::{AccessCheckInput, lifecycle::PublishEventInput},
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

/// Check collection-level access, returning a JSON error response on failure.
#[cfg(not(tarpaulin_include))]
pub fn check_upload_access(
    state: &AdminState,
    access: Option<&HookRef>,
    user_doc: Option<&Document>,
    id: Option<&str>,
    deny_msg: &str,
    operation: &str,
    collection: &str,
) -> Result<(), Box<Response>> {
    let db_kind = state.infra.pool.kind();
    let mut conn = state
        .infra
        .pool
        .get()
        .map_err(|e| Box::new(db_error_response(e, db_kind)))?;

    let tx = conn
        .transaction()
        .map_err(|e| Box::new(db_error_response(e, db_kind)))?;

    let result = state.infra.hook_runner.check_access(
        &AccessCheckInput::builder(operation, collection)
            .access(access)
            .user(user_doc)
            .id(id)
            .build(),
        &tx,
    );

    if let Err(e) = tx.commit() {
        warn!("tx commit failed: {e}");
    }

    match result {
        Ok(AccessResult::Denied) => Err(Box::new(json_error(StatusCode::FORBIDDEN, deny_msg))),
        Err(e) => {
            error!("Upload access check failed: {}", e);

            Err(Box::new(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Access check failed",
            )))
        }
        _ => Ok(()),
    }
}

/// The response to a failed connection checkout or transaction start: an
/// exhausted or busy pool is `503`, as every other surface reports it, and
/// anything else a generic `500`.
fn db_error_response(e: anyhow::Error, db_kind: &str) -> Response {
    service_error_to_response(&ServiceError::classify(e, db_kind))
}

/// Publish a mutation event and build the `EventUser` from auth.
#[cfg(not(tarpaulin_include))]
pub fn publish_upload_event(
    state: &AdminState,
    def: &CollectionDefinition,
    collection: impl Into<String>,
    doc_id: impl Into<String>,
    operation: EventOperation,
    data: Option<DocumentFields>,
    auth_user: Option<&AuthUser>,
) {
    let edited_by =
        auth_user.map(|au| EventUser::new(au.claims.sub.clone(), au.claims.email.clone()));

    // Create/update carry the full upload doc — derive the view from `_status`;
    // a delete carries no payload, so gate it by the collection's soft-delete
    // mode (upload collections have no status axis, so status stays `None`).
    let view = match &data {
        Some(d) => EventViewMeta::from_fields(d),
        None => EventViewMeta::for_delete(def.soft_delete, None),
    };

    let mut builder = PublishEventInput::builder(EventTarget::Collection, operation)
        .collection(collection.into())
        .document_id(doc_id.into())
        .edited_by(edited_by)
        .view(view);

    // Attach the payload only in `full` live mode — same stripping discipline as
    // the central publish path, so a `metadata`-mode collection never puts the
    // full upload document on the transport wire. (The `view` above was derived
    // from the full `data` first, which it must be.)
    if let Some(d) = data
        && def.live_mode == LiveMode::Full
    {
        builder = builder.data(d);
    }

    state.infra.hook_runner.publish_event(
        &state.infra.event_transport,
        &def.hooks,
        def.live.as_ref(),
        builder.build(),
    );
}

/// Map a [`ServiceError`] to the appropriate JSON error response.
///
/// Semantic errors (validation, access, hook, not-found, unique violation,
/// referenced, auth failures) surface their `Display` to the client — the
/// text is user-facing and kept sanitized by the service layer.
///
/// `Transient` and `Internal` wrap raw backend / pool errors whose `Display`
/// can leak DB identifiers or driver vocabulary. Those are logged at `error`
/// and the client receives a generic phrase only.
pub fn service_error_to_response(err: &ServiceError) -> Response {
    let (status, message) = match err {
        ServiceError::AccessDenied(_) => (StatusCode::FORBIDDEN, err.to_string()),
        ServiceError::NotFound(_) => (StatusCode::NOT_FOUND, err.to_string()),
        ServiceError::Validation(_) | ServiceError::HookError(_) => {
            (StatusCode::BAD_REQUEST, err.to_string())
        }
        // Same "refused due to data state" class as Referenced → 409.
        ServiceError::UniqueViolation(_)
        | ServiceError::Referenced { .. }
        | ServiceError::LimitExceeded(_) => (StatusCode::CONFLICT, err.to_string()),
        ServiceError::AccountLocked
        | ServiceError::EmailNotVerified
        | ServiceError::InvalidCredentials
        | ServiceError::InvalidToken { .. } => (StatusCode::UNAUTHORIZED, err.to_string()),
        ServiceError::Transient(_) => {
            error!("Upload service transient error: {}", err);
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
