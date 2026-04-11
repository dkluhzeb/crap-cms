//! DELETE /api/upload/{slug}/{id} — delete an upload document and its files.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde_json::json;
use tokio::task;

use crate::{
    admin::AdminState,
    core::event::EventOperation,
    db::query,
    service::{ServiceContext, delete_document},
};

use super::helpers::{
    check_upload_access, classify_delete_error, extract_bearer_user, json_error, json_ok,
    publish_upload_event,
};

#[cfg(not(tarpaulin_include))]
pub(super) async fn delete_upload(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let auth_user = match extract_bearer_user(&state, &headers) {
        Ok(u) => u,
        Err(e) => return *e,
    };

    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => {
            return json_error(
                StatusCode::NOT_FOUND,
                &format!("Collection '{}' not found", slug),
            );
        }
    };

    if !def.is_upload_collection() {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("Collection '{}' is not an upload collection", slug),
        );
    }

    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
    let access_fn = if def.soft_delete {
        def.access.resolve_trash()
    } else {
        def.access.delete.as_deref()
    };

    if let Err(resp) = check_upload_access(
        &state,
        access_fn,
        user_doc,
        Some(&id),
        "Delete access denied",
    ) {
        return *resp;
    }

    let doc_exists = state
        .pool
        .get()
        .ok()
        .and_then(|conn| {
            query::find_by_id(&conn, &slug, &def, &id, None)
                .ok()
                .flatten()
        })
        .is_some();

    if !doc_exists {
        return json_error(
            StatusCode::NOT_FOUND,
            &format!("Document '{}' not found", id),
        );
    }

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let def_clone = def.clone();
    let slug_owned = slug.clone();
    let id_owned = id.clone();
    let user_doc_owned = auth_user.as_ref().map(|au| au.user_doc.clone());
    let storage = state.storage.clone();
    let locale_config = state.config.locale.clone();

    let result = task::spawn_blocking(move || {
        let ctx = ServiceContext::collection(&slug_owned, &def_clone)
            .pool(&pool)
            .runner(&runner)
            .user(user_doc_owned.as_ref())
            .build();
        delete_document(&ctx, &id_owned, Some(&*storage), Some(&locale_config))
    })
    .await;

    match result {
        Ok(Ok(_req_context)) => {
            publish_upload_event(
                &state,
                &def,
                slug,
                id,
                EventOperation::Delete,
                None,
                &auth_user,
            );
            json_ok(StatusCode::OK, &json!({ "success": true }))
        }
        Ok(Err(e)) => {
            let msg = e.to_string();
            json_error(
                classify_delete_error(&msg),
                &format!("Delete error: {}", msg),
            )
        }
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task error: {}", e),
        ),
    }
}
