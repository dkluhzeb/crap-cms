//! PATCH /api/upload/{slug}/{id} — replace file on an existing document.

use std::{collections::HashMap, sync::Arc};

use axum::{
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use tokio::task;
use tracing::error;

use crate::{
    admin::{AdminState, handlers::shared::response::on_blocking_section, parse_multipart_form},
    core::{CollectionDefinition, Document, upload::UploadedFile},
    service::{
        AppInfra, ServiceContext, ServiceError,
        upload::{self, UpdateUploadInput, UploadUpdateResult},
    },
};

use super::helpers::{
    DocumentBody, check_upload_access, extract_bearer_user, json_error, json_ok,
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
        .build();

    // Recover the real error kind from a bare `Internal` before the HTTP mapper,
    // matching the gRPC/admin write paths (see `create_upload_blocking`).
    let db_kind = input.infra.pool.kind();

    upload::update_upload(
        &ctx,
        UpdateUploadInput {
            id: &input.id,
            storage: &input.infra.storage,
            file: input.file,
            form_data: input.form_data,
            ui_locale: input.ui_locale,
            locale_config: &input.infra.locale_config,
            upload_max_file_size: input.max_file_size,
            image_max_attempts: input.image_max_attempts,
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
    // Run the synchronous auth + Lua access gate on the blocking pool, not the
    // async worker. The multipart parse below stays async.
    let (auth_user, def) = match on_blocking_section(|| {
        let auth_user = extract_bearer_user(&state, &headers)?;

        let def = state
            .infra
            .registry
            .get_collection(&slug)
            .cloned()
            .ok_or_else(|| {
                Box::new(json_error(
                    StatusCode::NOT_FOUND,
                    &format!("Collection '{slug}' not found"),
                ))
            })?;

        if !def.is_upload_collection() {
            return Err(Box::new(json_error(
                StatusCode::BAD_REQUEST,
                &format!("Collection '{slug}' is not an upload collection"),
            )));
        }

        let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

        check_upload_access(
            &state,
            def.access.update.as_ref(),
            user_doc,
            Some(&id),
            "Update access denied",
            "update",
            &def.slug,
        )?;

        Ok((auth_user, def))
    }) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };

    let (form_data, file) = match parse_multipart_form(request, &state).await {
        Ok(result) => result,
        Err(e) => {
            error!("Upload multipart parse failed: {}", e);

            return json_error(StatusCode::BAD_REQUEST, "Invalid multipart request");
        }
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

    let result = task::spawn_blocking(move || update_upload_blocking(input)).await;

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
