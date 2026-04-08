//! Shared helpers for upload API handlers: auth, JSON responses, error classification.

use std::collections::HashMap;

use axum::{
    http::{
        HeaderMap, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::{
    admin::{AdminState, server::load_auth_user},
    core::{
        CollectionDefinition, Document,
        auth::{self, AuthUser},
        event::{EventOperation, EventTarget, EventUser},
    },
    db::AccessResult,
    hooks::lifecycle::PublishEventInput,
    service::ServiceError,
};

/// Extract Bearer token string from an Authorization header value.
pub fn extract_bearer_token(auth_header: &str) -> Option<&str> {
    auth_header
        .strip_prefix("Bearer ")
        .filter(|s| !s.is_empty())
}

/// Extract an authenticated user from the `Authorization: Bearer <jwt>` header.
///
/// Returns `Ok(None)` when no Authorization header is present (anonymous),
/// `Ok(Some(user))` for a valid token, or `Err(401)` for an invalid/expired token.
#[cfg(not(tarpaulin_include))]
pub fn extract_bearer_user(
    state: &AdminState,
    headers: &HeaderMap,
) -> Result<Option<AuthUser>, Box<Response>> {
    let auth_header = match headers.get(AUTHORIZATION) {
        Some(h) => match h.to_str() {
            Ok(s) => s,
            Err(_) => {
                return Err(Box::new(json_error(
                    StatusCode::UNAUTHORIZED,
                    "Invalid Authorization header",
                )));
            }
        },
        None => return Ok(None),
    };

    let token = match extract_bearer_token(auth_header) {
        Some(t) => t,
        None => {
            return Err(Box::new(json_error(
                StatusCode::UNAUTHORIZED,
                "Authorization header must use Bearer scheme",
            )));
        }
    };

    let claims = auth::validate_token(token, state.jwt_secret.as_ref()).map_err(|_| {
        Box::new(json_error(
            StatusCode::UNAUTHORIZED,
            "Invalid or expired token",
        ))
    })?;

    Ok(load_auth_user(
        &state.pool,
        &state.registry,
        &claims,
        &state.config.locale,
    ))
}

/// Return a JSON error response.
pub fn json_error(status: StatusCode, message: &str) -> Response {
    let body = json!({ "error": message });

    (
        status,
        [(CONTENT_TYPE, "application/json; charset=utf-8")],
        body.to_string(),
    )
        .into_response()
}

/// Classify a delete error message into the appropriate HTTP status code.
pub fn classify_delete_error(msg: &str) -> StatusCode {
    if msg.contains("not found") {
        StatusCode::NOT_FOUND
    } else if msg.contains("Cannot delete") || msg.contains("referenced by") {
        StatusCode::CONFLICT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

/// Return a JSON success response with the given status and body.
pub fn json_ok(status: StatusCode, body: &Value) -> Response {
    (
        status,
        [(CONTENT_TYPE, "application/json; charset=utf-8")],
        body.to_string(),
    )
        .into_response()
}

/// Check collection-level access, returning a JSON error response on failure.
#[cfg(not(tarpaulin_include))]
pub fn check_upload_access(
    state: &AdminState,
    access_ref: Option<&str>,
    user_doc: Option<&Document>,
    id: Option<&str>,
    deny_msg: &str,
) -> Result<(), Box<Response>> {
    let mut conn = state.pool.get().map_err(|_| {
        Box::new(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database error",
        ))
    })?;

    let tx = conn.transaction().map_err(|_| {
        Box::new(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database error",
        ))
    })?;

    let result = state
        .hook_runner
        .check_access(access_ref, user_doc, id, None, &tx);

    if let Err(e) = tx.commit() {
        tracing::warn!("tx commit failed: {e}");
    }

    match result {
        Ok(AccessResult::Denied) => Err(Box::new(json_error(StatusCode::FORBIDDEN, deny_msg))),
        Err(e) => Err(Box::new(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Access check error: {}", e),
        ))),
        _ => Ok(()),
    }
}

/// Publish a mutation event and build the EventUser from auth.
#[cfg(not(tarpaulin_include))]
pub fn publish_upload_event(
    state: &AdminState,
    def: &CollectionDefinition,
    collection: impl Into<String>,
    doc_id: impl Into<String>,
    operation: EventOperation,
    data: Option<HashMap<String, Value>>,
    auth_user: &Option<AuthUser>,
) {
    let edited_by = auth_user
        .as_ref()
        .map(|au| EventUser::new(au.claims.sub.clone(), au.claims.email.clone()));

    let mut builder = PublishEventInput::builder(EventTarget::Collection, operation)
        .collection(collection.into())
        .document_id(doc_id.into())
        .edited_by(edited_by);

    if let Some(d) = data {
        builder = builder.data(d);
    }

    state.hook_runner.publish_event(
        &state.event_bus,
        &def.hooks,
        def.live.as_ref(),
        builder.build(),
    );
}

/// Map a [`ServiceError`] to the appropriate JSON error response.
pub fn service_error_to_response(err: ServiceError) -> Response {
    let (status, message) = match &err {
        ServiceError::AccessDenied(_) => (StatusCode::FORBIDDEN, err.to_string()),
        ServiceError::NotFound(_) => (StatusCode::NOT_FOUND, err.to_string()),
        ServiceError::Validation(_) => (StatusCode::BAD_REQUEST, err.to_string()),
        ServiceError::HookError(_) => (StatusCode::BAD_REQUEST, err.to_string()),
        ServiceError::UniqueViolation(_) => (StatusCode::CONFLICT, err.to_string()),
        ServiceError::Referenced { .. } => (StatusCode::CONFLICT, err.to_string()),
        ServiceError::AccountLocked
        | ServiceError::EmailNotVerified
        | ServiceError::InvalidCredentials => (StatusCode::UNAUTHORIZED, err.to_string()),
        ServiceError::InvalidToken { .. } => (StatusCode::UNAUTHORIZED, err.to_string()),
        ServiceError::Transient(_) => (StatusCode::SERVICE_UNAVAILABLE, err.to_string()),
        ServiceError::Internal(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error".to_string(),
        ),
    };

    json_error(status, &message)
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use serde_json::json;

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
        let body = json!({ "success": true });
        let resp = json_ok(StatusCode::CREATED, &body);
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn json_ok_body_matches() {
        let body_val = json!({ "document": { "id": "abc" } });
        let resp = json_ok(StatusCode::OK, &body_val);
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

    #[test]
    fn delete_error_not_found() {
        assert_eq!(
            classify_delete_error("Document not found in collection"),
            StatusCode::NOT_FOUND,
        );
    }

    #[test]
    fn delete_error_referenced() {
        assert_eq!(
            classify_delete_error(
                "Cannot delete: this document is referenced by 3 other document(s)"
            ),
            StatusCode::CONFLICT,
        );
    }

    #[test]
    fn delete_error_referenced_by_keyword() {
        assert_eq!(
            classify_delete_error("Still referenced by other documents"),
            StatusCode::CONFLICT,
        );
    }

    #[test]
    fn delete_error_generic() {
        assert_eq!(
            classify_delete_error("Database write failed"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    // ── service_error_to_response ──────────────────────────────────

    #[tokio::test]
    async fn service_error_access_denied_returns_403() {
        let resp = service_error_to_response(ServiceError::AccessDenied("nope".into()));
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn service_error_not_found_returns_404() {
        let resp = service_error_to_response(ServiceError::NotFound("gone".into()));
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn service_error_validation_returns_400() {
        use crate::core::validate::{FieldError, ValidationError};
        let ve = ValidationError::new(vec![FieldError::new("title", "required")]);
        let resp = service_error_to_response(ServiceError::Validation(ve));
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn service_error_hook_error_returns_400() {
        let resp = service_error_to_response(ServiceError::HookError("bad hook".into()));
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn service_error_unique_violation_returns_409() {
        let resp = service_error_to_response(ServiceError::UniqueViolation("email".into()));
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn service_error_referenced_returns_409() {
        let resp = service_error_to_response(ServiceError::Referenced {
            id: "doc-1".into(),
            count: 3,
        });
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn service_error_internal_returns_500_generic_message() {
        let resp =
            service_error_to_response(ServiceError::Internal(anyhow::anyhow!("secret details")));
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["error"], "Internal server error");
    }

    #[tokio::test]
    async fn service_error_transient_returns_503() {
        let resp = service_error_to_response(ServiceError::Transient(anyhow::anyhow!("db locked")));
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn service_error_account_locked_returns_401() {
        let resp = service_error_to_response(ServiceError::AccountLocked);
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn service_error_invalid_token_returns_401() {
        let resp = service_error_to_response(ServiceError::InvalidToken {
            kind: "reset",
            reason: "expired",
        });
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
