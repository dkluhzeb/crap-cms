//! Delete handler — soft-delete or permanent deletion of collection items.

use axum::{
    Extension, Json,
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};
use serde_json::json;
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{forbidden, get_event_user, get_user_doc, htmx_redirect},
    },
    core::{
        auth::AuthUser,
        event::{EventOperation, EventTarget},
    },
    hooks::lifecycle::PublishEventInput,
    service::{self, ServiceError},
};

/// Build a JSON `{"ok": true}` success response.
fn json_ok_response() -> Response {
    Json(json!({"ok": true})).into_response()
}

/// Build a JSON `{"error": "..."}` error response with 400 status.
fn json_error_response(msg: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response()
}

/// DELETE handler for collection items (called from `delete_action.rs`).
pub(in crate::admin::handlers::collections) async fn delete_action_impl(
    state: &AdminState,
    slug: &str,
    id: &str,
    auth_user: &Option<Extension<AuthUser>>,
    force_hard_delete: bool,
    json_response: bool,
) -> Response {
    let def = match state.registry.get_collection(slug) {
        Some(d) => d.clone(),
        None => {
            if json_response {
                return json_error_response("Collection not found");
            }

            return Redirect::to("/admin/collections").into_response();
        }
    };

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let mut def_clone = def.clone();
    let slug_owned = slug.to_string();
    let id_owned = id.to_string();
    let user_doc = get_user_doc(auth_user).cloned();
    let storage = state.storage.clone();
    let locale_config = state.config.locale.clone();

    if force_hard_delete {
        def_clone.soft_delete = false;
    }

    let result = task::spawn_blocking(move || {
        service::delete_document(
            &pool,
            &runner,
            &slug_owned,
            &id_owned,
            &def_clone,
            user_doc.as_ref(),
            Some(&*storage),
            Some(&locale_config),
        )
    })
    .await;

    match result {
        Ok(Ok(_)) => {
            state.hook_runner.publish_event(
                &state.event_bus,
                &def.hooks,
                def.live.as_ref(),
                PublishEventInput::builder(EventTarget::Collection, EventOperation::Delete)
                    .collection(slug.to_string())
                    .document_id(id.to_string())
                    .edited_by(get_event_user(auth_user))
                    .build(),
            );

            if json_response {
                return json_ok_response();
            }
        }
        Ok(Err(e)) => {
            let msg = match &e {
                ServiceError::AccessDenied(_) => {
                    let deny_msg = if def.soft_delete && !force_hard_delete {
                        "You don't have permission to trash this item"
                    } else {
                        "You don't have permission to permanently delete this item"
                    };

                    if json_response {
                        return json_error_response(deny_msg);
                    }

                    return forbidden(state, deny_msg).into_response();
                }
                ServiceError::Referenced { count, .. } => {
                    format!("Cannot delete: referenced by {count} document(s)")
                }
                _ => {
                    error!("Delete error: {}", e);
                    "Failed to delete item".to_string()
                }
            };

            if json_response {
                return json_error_response(&msg);
            }
        }
        Err(e) => {
            error!("Delete task error: {}", e);

            if json_response {
                return json_error_response("Failed to delete item");
            }
        }
    }

    htmx_redirect(&format!("/admin/collections/{}", slug))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::Access;

    #[test]
    fn trash_access_falls_back_to_update() {
        let access = Access {
            trash: Some("access.trash_fn".to_string()),
            update: Some("access.update_fn".to_string()),
            ..Default::default()
        };
        assert_eq!(access.resolve_trash(), Some("access.trash_fn"));

        let access = Access {
            trash: None,
            update: Some("access.update_fn".to_string()),
            ..Default::default()
        };
        assert_eq!(access.resolve_trash(), Some("access.update_fn"));

        let access = Access::default();
        assert!(access.resolve_trash().is_none());
    }

    #[test]
    fn json_ok_response_returns_200() {
        let resp = json_ok_response();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn json_error_response_returns_400() {
        let resp = json_error_response("something went wrong");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
