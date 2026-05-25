//! PATCH /api/upload/{slug}/{id} — replace file on an existing document.

use tracing::error;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use tokio::task;

use std::collections::HashMap;

use crate::{
    admin::AdminState,
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document, SharedStorage, event::EventOperation, upload::UploadedFile,
    },
    db::DbPool,
    hooks::HookRunner,
    service::{self, ServiceError, upload::UploadUpdateResult},
};

/// Owned bundle for the upload-update spawn-blocking body.
struct UploadUpdateBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    storage: SharedStorage,
    slug: String,
    id: String,
    def: CollectionDefinition,
    user_doc: Option<Document>,
    file: Option<UploadedFile>,
    form_data: HashMap<String, String>,
    ui_locale: Option<String>,
    locale_config: LocaleConfig,
    max_file_size: u64,
    image_max_attempts: u32,
}

fn update_upload_blocking(
    input: UploadUpdateBlockingInput,
) -> Result<UploadUpdateResult, ServiceError> {
    let ctx = service::ServiceContext::collection(&input.slug, &input.def)
        .pool(&input.pool)
        .runner(&input.runner)
        .user(input.user_doc.as_ref())
        .build();

    service::upload::update_upload(
        &ctx,
        service::upload::UpdateUploadInput {
            id: &input.id,
            storage: &input.storage,
            file: input.file,
            form_data: input.form_data,
            ui_locale: input.ui_locale,
            locale_config: &input.locale_config,
            upload_max_file_size: input.max_file_size,
            image_max_attempts: input.image_max_attempts,
        },
    )
}

use super::helpers::{
    DocumentBody, check_upload_access, extract_bearer_user, json_error, json_ok,
    publish_upload_event, service_error_to_response,
};
use crate::admin::parse_multipart_form;

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

    let Some(def) = state.registry.get_collection(&slug).cloned() else {
        return json_error(
            StatusCode::NOT_FOUND,
            &format!("Collection '{slug}' not found"),
        );
    };

    if !def.is_upload_collection() {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("Collection '{slug}' is not an upload collection"),
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

    let input = UploadUpdateBlockingInput {
        pool: state.pool.clone(),
        runner: state.hook_runner.clone(),
        storage: state.storage.clone(),
        slug: slug.clone(),
        id: id.clone(),
        def: def.clone(),
        user_doc: auth_user.as_ref().map(|au| au.user_doc.clone()),
        file,
        form_data,
        ui_locale: auth_user.as_ref().map(|au| au.ui_locale.clone()),
        locale_config: state.config.locale.clone(),
        max_file_size: state.config.upload.max_file_size,
        image_max_attempts: state.config.jobs.system_image_max_attempts(),
    };

    let result = task::spawn_blocking(move || update_upload_blocking(input)).await;

    match result {
        Ok(Ok(UploadUpdateResult { doc, .. })) => {
            publish_upload_event(
                &state,
                &def,
                slug,
                id,
                EventOperation::Update,
                Some(doc.fields.clone()),
                auth_user.as_ref(),
            );

            json_ok(StatusCode::OK, &DocumentBody { document: &doc })
        }
        Ok(Err(e)) => service_error_to_response(&e),
        Err(e) => {
            error!("Upload update task join failed: {}", e);

            json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        }
    }
}
