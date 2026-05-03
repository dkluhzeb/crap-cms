//! PATCH /api/upload/{slug}/{id} — replace file on an existing document.

use tracing::error;

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
    service::{self, upload::UploadUpdateResult},
};

use super::helpers::{
    check_upload_access, extract_bearer_user, json_error, json_ok, publish_upload_event,
    service_error_to_response,
};
use crate::admin::handlers::forms::parse_multipart_form;

#[cfg(not(tarpaulin_include))]
pub(super) async fn update_upload(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    headers: HeaderMap,
    request: axum::extract::Request,
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

    // Defense-in-depth: pre-check access before parsing the multipart body.
    if let Err(resp) = check_upload_access(
        &state,
        def.access.update.as_deref(),
        user_doc,
        Some(&id),
        "Update access denied",
    ) {
        return *resp;
    }

    let (form_data, file) = match parse_multipart_form(request, &state).await {
        Ok(result) => result,
        Err(e) => {
            error!("Upload multipart parse failed: {}", e);

            return json_error(StatusCode::BAD_REQUEST, "Invalid multipart request");
        }
    };

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let storage = state.storage.clone();
    let slug_owned = slug.clone();
    let id_owned = id.clone();
    let def_owned = def.clone();
    let user_doc_owned = auth_user.as_ref().map(|au| au.user_doc.clone());
    let ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
    let locale_config = state.config.locale.clone();
    let max_file_size = state.config.upload.max_file_size;

    let result = task::spawn_blocking(move || {
        let ctx = service::ServiceContext::collection(&slug_owned, &def_owned)
            .pool(&pool)
            .runner(&runner)
            .user(user_doc_owned.as_ref())
            .build();
        service::upload::update_upload(
            &ctx,
            service::upload::UpdateUploadInput {
                id: &id_owned,
                storage: &storage,
                file,
                form_data,
                ui_locale,
                locale_config: &locale_config,
                upload_max_file_size: max_file_size,
            },
        )
    })
    .await;

    match result {
        Ok(Ok(UploadUpdateResult { doc, .. })) => {
            publish_upload_event(
                &state,
                &def,
                slug,
                id,
                EventOperation::Update,
                Some(doc.fields.clone()),
                &auth_user,
            );

            json_ok(StatusCode::OK, &json!({ "document": doc }))
        }
        Ok(Err(e)) => service_error_to_response(e),
        Err(e) => {
            error!("Upload update task join failed: {}", e);

            json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        }
    }
}
