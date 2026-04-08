//! POST /api/upload/{slug} — upload a file and create a document.

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
    service::{self, upload::UploadCreateResult},
};

use super::helpers::{
    check_upload_access, extract_bearer_user, json_error, json_ok, publish_upload_event,
    service_error_to_response,
};
use crate::admin::handlers::forms::parse_multipart_form;

#[cfg(not(tarpaulin_include))]
pub(super) async fn create_upload(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
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
        def.access.create.as_deref(),
        user_doc,
        None,
        "Create access denied",
    ) {
        return *resp;
    }

    let (form_data, file) = match parse_multipart_form(request, &state).await {
        Ok(result) => result,
        Err(e) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                &format!("Multipart parse error: {}", e),
            );
        }
    };

    let file = match file {
        Some(f) => f,
        None => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "No file provided (use field name '_file')",
            );
        }
    };

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let storage = state.storage.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let user_doc_owned = auth_user.as_ref().map(|au| au.user_doc.clone());
    let ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
    let max_file_size = state.config.upload.max_file_size;

    let result = task::spawn_blocking(move || {
        service::upload::create_upload(
            &pool,
            &runner,
            &storage,
            &slug_owned,
            &def_owned,
            file,
            form_data,
            user_doc_owned.as_ref(),
            ui_locale,
            max_file_size,
        )
    })
    .await;

    match result {
        Ok(Ok(UploadCreateResult { doc, .. })) => {
            publish_upload_event(
                &state,
                &def,
                slug,
                doc.id.clone(),
                EventOperation::Create,
                Some(doc.fields.clone()),
                &auth_user,
            );

            json_ok(StatusCode::CREATED, &json!({ "document": doc }))
        }
        Ok(Err(e)) => service_error_to_response(e),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task error: {}", e),
        ),
    }
}
