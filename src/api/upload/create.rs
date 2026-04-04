//! POST /api/upload/{slug} — upload a file and create a document.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde_json::json;
use tokio::task;
use tracing::warn;

use crate::{
    admin::AdminState,
    core::{
        event::EventOperation,
        upload::{self, inject_upload_metadata},
    },
    service::{self, WriteInput},
};

use super::helpers::{
    check_upload_access, extract_bearer_user, json_error, json_ok, publish_upload_event,
    strip_read_denied_doc_fields, strip_write_denied_fields,
};
use crate::admin::handlers::forms::{extract_join_data_from_form, parse_multipart_form};

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

    if let Err(resp) = check_upload_access(
        &state,
        def.access.create.as_deref(),
        user_doc,
        None,
        "Create access denied",
    ) {
        return *resp;
    }

    let (mut form_data, file) = match parse_multipart_form(request, &state).await {
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

    let upload_config = match def.upload.as_ref() {
        Some(c) => c.clone(),
        None => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Upload config missing"),
    };

    let storage = state.storage.clone();
    let slug_for_upload = slug.clone();
    let global_max = state.config.upload.max_file_size;

    let (processed, mut guard) = match task::spawn_blocking(move || {
        upload::process_upload(file, &upload_config, storage, &slug_for_upload, global_max)
    })
    .await
    {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return json_error(StatusCode::BAD_REQUEST, &e.to_string()),
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Task error: {}", e),
            );
        }
    };

    let queued_conversions = processed.queued_conversions.clone();
    inject_upload_metadata(&mut form_data, &processed);

    strip_write_denied_fields(&state, &def.fields, user_doc, "create", &mut form_data);

    let password = if def.is_auth_collection() {
        form_data.remove("password")
    } else {
        None
    };
    let join_data = extract_join_data_from_form(&form_data, &def.fields);
    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let user_doc_owned = auth_user.as_ref().map(|au| au.user_doc.clone());
    let ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());

    let result = task::spawn_blocking(move || {
        service::create_document(
            &pool,
            &runner,
            &slug_owned,
            &def_owned,
            WriteInput::builder(form_data, &join_data)
                .password(password.as_deref())
                .draft(draft)
                .ui_locale(ui_locale)
                .build(),
            user_doc_owned.as_ref(),
        )
    })
    .await;

    match result {
        Ok(Ok((mut doc, _req_context))) => {
            guard.commit();

            if !queued_conversions.is_empty()
                && let Ok(conn) = state.pool.get()
                && let Err(e) =
                    upload::enqueue_conversions(&conn, &slug, &doc.id, &queued_conversions)
            {
                warn!("Failed to enqueue image conversions: {}", e);
            }

            strip_read_denied_doc_fields(&state, &def.fields, &auth_user, &mut doc.fields);

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
        Ok(Err(e)) => json_error(StatusCode::BAD_REQUEST, &e.to_string()),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task error: {}", e),
        ),
    }
}
