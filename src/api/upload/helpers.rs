//! Shared helpers for upload API handlers: auth, JSON responses, error classification.

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
    core::auth::{self, AuthUser},
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
    user_doc: Option<&crate::core::Document>,
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
        Ok(crate::db::AccessResult::Denied) => {
            Err(Box::new(json_error(StatusCode::FORBIDDEN, deny_msg)))
        }
        Err(e) => Err(Box::new(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Access check error: {}", e),
        ))),
        _ => Ok(()),
    }
}

/// Strip field-level write-denied fields from form data.
#[cfg(not(tarpaulin_include))]
pub fn strip_write_denied_fields(
    state: &AdminState,
    fields: &[crate::core::FieldDefinition],
    user_doc: Option<&crate::core::Document>,
    operation: &str,
    form_data: &mut std::collections::HashMap<String, String>,
) {
    if let Ok(mut conn) = state.pool.get()
        && let Ok(tx) = conn.transaction()
    {
        let denied = state
            .hook_runner
            .check_field_write_access(fields, user_doc, operation, &tx);

        if let Err(e) = tx.commit() {
            tracing::warn!("tx commit failed: {e}");
        }

        for name in &denied {
            form_data.remove(name);
        }
    }
}

/// Strip field-level read-denied fields from a document's fields map (for JSON responses).
#[cfg(not(tarpaulin_include))]
pub fn strip_read_denied_doc_fields(
    state: &AdminState,
    fields: &[crate::core::FieldDefinition],
    auth_user: &Option<crate::core::auth::AuthUser>,
    doc_fields: &mut std::collections::HashMap<String, serde_json::Value>,
) {
    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

    if let Ok(mut conn) = state.pool.get()
        && let Ok(tx) = conn.transaction()
    {
        let denied = state
            .hook_runner
            .check_field_read_access(fields, user_doc, &tx);

        let _ = tx.commit();

        for name in &denied {
            doc_fields.remove(name);
        }
    }
}

/// Publish a mutation event and build the EventUser from auth.
#[cfg(not(tarpaulin_include))]
pub fn publish_upload_event(
    state: &AdminState,
    def: &crate::core::CollectionDefinition,
    collection: impl Into<String>,
    doc_id: impl Into<String>,
    operation: crate::core::event::EventOperation,
    data: Option<std::collections::HashMap<String, serde_json::Value>>,
    auth_user: &Option<crate::core::auth::AuthUser>,
) {
    use crate::core::event::{EventTarget, EventUser};
    use crate::hooks::lifecycle::PublishEventInput;

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
}
