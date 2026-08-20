//! POST /api/upload/{slug} — upload a file and create a document.

use tracing::error;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use tokio::task;

use std::collections::HashMap;

use crate::core::SharedStorage;
use crate::{
    admin::AdminState,
    core::{CollectionDefinition, Document, event::EventOperation, upload::UploadedFile},
    db::DbPool,
    hooks::HookRunner,
    service::{self, ServiceError, upload::UploadCreateResult},
};

use super::helpers::{
    DocumentBody, check_upload_access, extract_bearer_user, json_error, json_ok,
    publish_upload_event, service_error_to_response,
};
use crate::admin::parse_multipart_form;

/// Owned bundle for the upload-create spawn-blocking body.
struct UploadCreateBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    storage: SharedStorage,
    slug: String,
    def: CollectionDefinition,
    user_doc: Option<Document>,
    file: UploadedFile,
    form_data: HashMap<String, String>,
    ui_locale: Option<String>,
    max_file_size: u64,
    image_max_attempts: u32,
}

fn create_upload_blocking(
    input: UploadCreateBlockingInput,
) -> Result<UploadCreateResult, ServiceError> {
    let ctx = service::ServiceContext::collection(&input.slug, &input.def)
        .pool(&input.pool)
        .runner(&input.runner)
        .user(input.user_doc.as_ref())
        .build();

    // Recover the real error kind (unique violation, transient lock, …) from a
    // bare `Internal` before it reaches the HTTP mapper, exactly as the gRPC and
    // admin write paths do — otherwise a conflict/retryable error is reported as
    // a generic 500.
    let db_kind = input.pool.kind();
    service::upload::create_upload(
        &ctx,
        &input.storage,
        &input.file,
        input.form_data,
        input.ui_locale,
        input.max_file_size,
        input.image_max_attempts,
    )
    .map_err(|e| e.reclassify(db_kind))
}

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
        def.access.create.as_ref(),
        user_doc,
        None,
        "Create access denied",
        "create",
        &def.slug,
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

    let Some(file) = file else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "No file provided (use field name '_file')",
        );
    };

    let input = UploadCreateBlockingInput {
        pool: state.pool.clone(),
        runner: state.hook_runner.clone(),
        storage: state.storage.clone(),
        slug: slug.clone(),
        def: def.clone(),
        user_doc: auth_user.as_ref().map(|au| au.user_doc.clone()),
        file,
        form_data,
        ui_locale: auth_user.as_ref().map(|au| au.ui_locale.clone()),
        max_file_size: state.config.upload.max_file_size,
        image_max_attempts: state.config.jobs.system_image_max_attempts(),
    };

    let result = task::spawn_blocking(move || create_upload_blocking(input)).await;

    match result {
        Ok(Ok(UploadCreateResult { doc, .. })) => {
            publish_upload_event(
                &state,
                &def,
                slug,
                doc.id.clone(),
                EventOperation::Create,
                Some(doc.fields.clone()),
                auth_user.as_ref(),
            );

            json_ok(StatusCode::CREATED, &DocumentBody { document: &doc })
        }
        Ok(Err(e)) => service_error_to_response(&e),
        Err(e) => {
            error!("Upload create task join failed: {}", e);

            json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        }
    }
}
