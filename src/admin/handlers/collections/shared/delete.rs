//! Delete handler — soft-delete or permanent deletion of collection items.

use axum::{
    Extension, Json,
    response::{IntoResponse, Redirect, Response},
};
use serde_json::json;
use tracing::error;

use std::sync::Arc;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{
            forbidden, get_user_doc, htmx_redirect, json_conflict, json_forbidden, json_not_found,
            json_server_error, paths,
        },
    },
    core::AuthUser,
    service::{
        ServiceError,
        op::{self, Delete, DeleteArgs, Principal, TargetRef},
    },
};

/// Build a JSON `{"ok": true}` success response.
fn json_ok_response() -> Response {
    Json(json!({"ok": true})).into_response()
}

/// DELETE handler for collection items (called from `delete_action.rs`).
pub(in crate::admin::handlers::collections) async fn delete_action_impl(
    state: &AdminState,
    slug: &str,
    id: &str,
    auth_user: Option<&Extension<AuthUser>>,
    force_hard_delete: bool,
    json_response: bool,
) -> Response {
    let Some(def) = state.infra.registry.get_collection(slug).cloned() else {
        if json_response {
            return json_not_found("Collection not found");
        }

        return Redirect::to(paths::COLLECTIONS_ROOT).into_response();
    };

    // `force_hard_delete` is expressed by the operation body via
    // `adjust_collection_def` — the definition-clone trick previously
    // copy-pasted on every surface.
    let args = DeleteArgs::builder(id)
        .force_hard_delete(force_hard_delete)
        .build();

    let result = op::run_blocking::<Delete>(
        Arc::clone(&state.infra),
        Principal::Resolved {
            user: get_user_doc(auth_user).cloned(),
            ui_locale: auth_user.map(|Extension(au)| au.ui_locale.clone()),
        },
        TargetRef::collection(slug),
        args,
    )
    .await;

    match result {
        Ok(_) => {
            if json_response {
                return json_ok_response();
            }
        }
        Err(core_err) => match core_err.into_service_error() {
            ServiceError::AccessDenied(_) => {
                let deny_msg = if def.soft_delete && !force_hard_delete {
                    "You don't have permission to trash this item"
                } else {
                    "You don't have permission to permanently delete this item"
                };

                if json_response {
                    return json_forbidden(deny_msg);
                }

                return forbidden(state, deny_msg).into_response();
            }
            ServiceError::Referenced { count, .. } => {
                // A precondition conflict, not a client input error.
                if json_response {
                    return json_conflict(&format!(
                        "Cannot delete: referenced by {count} document(s)"
                    ));
                }
            }
            e => {
                error!("Delete error: {}", e);

                if json_response {
                    return json_server_error("Failed to delete item");
                }
            }
        },
    }

    htmx_redirect(&paths::collection(slug))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::HookRef;
    use crate::core::collection::Access;
    use axum::http::StatusCode;

    #[test]
    fn trash_access_falls_back_to_update() {
        let access = Access {
            trash: Some(HookRef::new("access.trash_fn")),
            update: Some(HookRef::new("access.update_fn")),
            ..Default::default()
        };
        assert_eq!(
            access.resolve_trash(),
            Some(&HookRef::new("access.trash_fn"))
        );

        let access = Access {
            trash: None,
            update: Some(HookRef::new("access.update_fn")),
            ..Default::default()
        };
        assert_eq!(
            access.resolve_trash(),
            Some(&HookRef::new("access.update_fn"))
        );

        let access = Access::default();
        assert!(access.resolve_trash().is_none());
    }

    #[test]
    fn json_ok_response_returns_200() {
        let resp = json_ok_response();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
