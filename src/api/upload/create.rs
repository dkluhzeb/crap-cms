//! POST /api/upload/{slug} — upload a file and create a document.

use std::{collections::HashMap, sync::Arc};

use axum::{
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState, FormData, handlers::shared::response::on_blocking_section, parse_multipart_form,
    },
    core::{CollectionDefinition, Document, upload::UploadedFile},
    service::{
        AppInfra, ServiceContext, ServiceError,
        upload::{CreateUploadInput, UploadCreateResult, create_upload as create_upload_document},
    },
};

use super::helpers::{
    DocumentBody, json_error, json_ok, multipart_error_response, resolve_upload_request,
    service_error_to_response,
};

/// Owned bundle for the upload-create spawn-blocking body. Storage, locale
/// config and transports come from `infra`.
struct UploadCreateBlockingInput {
    infra: Arc<AppInfra>,
    slug: String,
    def: Arc<CollectionDefinition>,
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
    let ctx = ServiceContext::collection(&input.slug, &input.def)
        .infra(&input.infra)
        .user(input.user_doc.as_ref())
        .ui_locale(input.ui_locale.clone())
        .build();

    // Recover the real error kind (unique violation, transient lock, …) from a
    // bare `Internal` before it reaches the HTTP mapper, exactly as the gRPC and
    // admin write paths do — otherwise a conflict/retryable error is reported as
    // a generic 500.
    let db_kind = input.infra.pool.kind();

    let mut form = FormData::from_raw(input.form_data, &input.def.fields);
    let draft = form.take_action() == "save_draft";
    let password = form.take_password(&input.def);

    create_upload_document(
        &ctx,
        CreateUploadInput {
            storage: &input.infra.storage,
            file: &input.file,
            form,
            locale_ctx: None,
            password,
            draft,
            upload_max_file_size: input.max_file_size,
            image_max_attempts: input.image_max_attempts,
        },
    )
    .map_err(|e| e.reclassify(db_kind))
}

#[cfg(not(tarpaulin_include))]
pub(super) async fn create_upload(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
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

    let Some(file) = file else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "No file provided (use field name '_file')",
        );
    };

    let input = UploadCreateBlockingInput {
        infra: state.infra.clone(),
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
            json_ok(StatusCode::CREATED, &DocumentBody { document: &doc })
        }
        Ok(Err(e)) => service_error_to_response(&e),
        Err(e) => {
            error!("Upload create task join failed: {}", e);

            json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{api::upload::helpers::test_support, core::event::EventOperation};

    /// An upload created through the REST API is written by the service with
    /// the app infra: its storage, one create event and a cleared populate
    /// cache.
    #[test]
    fn an_api_upload_is_created_through_the_service() {
        let (_tmp, infra, mut rx) = test_support::infra_with_events();
        let def = infra.registry.get_collection("media").cloned().unwrap();

        let input = UploadCreateBlockingInput {
            infra: Arc::clone(&infra),
            slug: "media".to_string(),
            def,
            user_doc: None,
            file: UploadedFile {
                filename: "a.txt".to_string(),
                content_type: "text/plain".to_string(),
                data: b"hello".to_vec(),
            },
            form_data: HashMap::new(),
            ui_locale: None,
            max_file_size: 1024 * 1024,
            image_max_attempts: 1,
        };

        let created = create_upload_blocking(input).expect("create upload");

        assert!(
            created.doc.get_str("filename").is_some(),
            "{:?}",
            created.doc
        );
        assert!(
            !infra.cache.has(test_support::CACHED_KEY).unwrap(),
            "the populate cache must be cleared"
        );

        let event = rx.try_recv().expect("a create event");
        assert!(matches!(event.operation, EventOperation::Create));
        assert_eq!(event.document_id, created.doc.id);
        assert!(rx.try_recv().is_err(), "exactly one event");
    }
}
