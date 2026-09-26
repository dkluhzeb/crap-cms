//! PATCH /api/upload/{slug}/{id} — replace file on an existing document.

use std::{collections::HashMap, sync::Arc};

use axum::{
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use tracing::error;

use crate::{
    admin::{
        AdminState, FormData, handlers::shared::response::on_blocking_section, parse_multipart_form,
    },
    core::{CollectionDefinition, Document, spawn_request_blocking, upload::UploadedFile},
    service::{
        AppInfra, ServiceContext, ServiceError,
        upload::{UpdateUploadInput, UploadUpdateResult, update_upload as update_upload_document},
    },
};

use super::helpers::{
    DocumentBody, json_error, json_ok, multipart_error_response, resolve_upload_request,
    service_error_to_response,
};

/// Owned bundle for the upload-update spawn-blocking body. Storage, locale
/// config and transports come from `infra`.
struct UploadUpdateBlockingInput {
    infra: Arc<AppInfra>,
    slug: String,
    id: String,
    def: Arc<CollectionDefinition>,
    user_doc: Option<Document>,
    file: Option<UploadedFile>,
    form_data: HashMap<String, String>,
    ui_locale: Option<String>,
    max_file_size: u64,
    image_max_attempts: u32,
}

fn update_upload_blocking(
    input: UploadUpdateBlockingInput,
) -> Result<UploadUpdateResult, ServiceError> {
    let ctx = ServiceContext::collection(&input.slug, &input.def)
        .infra(&input.infra)
        .user(input.user_doc.as_ref())
        .ui_locale(input.ui_locale.clone())
        .build();

    // Recover the real error kind from a bare `Internal` before the HTTP mapper,
    // matching the gRPC/admin write paths (see `create_upload_blocking`).
    let db_kind = input.infra.pool.kind();

    let mut form = FormData::from_raw(input.form_data, &input.def.fields);
    let draft = form.take_action() == "save_draft";
    let password = form.take_password(&input.def);
    let expected_revision = form.take_revision().map_err(ServiceError::HookError)?;

    update_upload_document(
        &ctx,
        UpdateUploadInput {
            id: &input.id,
            storage: &input.infra.storage,
            file: input.file,
            form,
            locale_ctx: None,
            password,
            draft,
            upload_max_file_size: input.max_file_size,
            image_max_attempts: input.image_max_attempts,
            // A programmatic caller spells out the fields it means to write, so
            // a shared field under a non-default locale stays a rejected write
            // rather than a silently dropped value.
            form_echoes_locked_fields: false,
            expected_revision,
        },
    )
    .map_err(|e| e.reclassify(db_kind))
}

#[cfg(not(tarpaulin_include))]
pub(super) async fn update_upload(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    headers: HeaderMap,
    request: Request,
) -> Response {
    // Auth (a read-pool checkout + queries) is synchronous and must not park
    // an async worker — run it on the blocking pool. The collection's access
    // rule is judged by the service, on the request's data.
    let (auth_user, def) =
        match on_blocking_section(|| resolve_upload_request(&state, &headers, &slug)) {
            Ok(v) => v,
            Err(resp) => return *resp,
        };

    let (form_data, file) = match parse_multipart_form(request, &state).await {
        Ok(result) => result,
        Err(e) => return multipart_error_response(&e),
    };

    let input = UploadUpdateBlockingInput {
        infra: state.infra.clone(),
        slug: slug.clone(),
        id: id.clone(),
        def: def.clone(),
        user_doc: auth_user.as_ref().map(|au| au.user_doc.clone()),
        file,
        form_data,
        ui_locale: auth_user.as_ref().map(|au| au.ui_locale.clone()),
        max_file_size: state.config.upload.max_file_size,
        image_max_attempts: state.config.jobs.system_image_max_attempts(),
    };

    let result = spawn_request_blocking(move || update_upload_blocking(input)).await;

    match result {
        Ok(Ok(UploadUpdateResult { doc, .. })) => {
            json_ok(StatusCode::OK, &DocumentBody { document: &doc })
        }
        Ok(Err(e)) => service_error_to_response(&e),
        Err(e) => {
            error!("Upload update task join failed: {}", e);

            json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        }
    }
}
