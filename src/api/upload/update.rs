//! PATCH /api/upload/{slug}/{id} — replace file on an existing document.

use std::collections::HashMap;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde_json::{Value, json};
use tokio::task;
use tracing::warn;

use crate::{
    admin::AdminState,
    core::{
        event::EventOperation,
        upload::{
            CleanupGuard, delete_upload_files, enqueue_conversions, inject_upload_metadata,
            process_upload,
        },
    },
    db::{LocaleContext, query},
    service::{WriteInput, update_document},
};

use super::helpers::{
    check_upload_access, extract_bearer_user, json_error, json_ok, publish_upload_event,
    strip_read_denied_doc_fields, strip_write_denied_fields,
};
use crate::admin::handlers::forms::{extract_join_data_from_form, parse_multipart_form};

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

    if let Err(resp) = check_upload_access(
        &state,
        def.access.update.as_deref(),
        user_doc,
        Some(&id),
        "Update access denied",
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

    let mut old_doc_fields: Option<HashMap<String, Value>> = None;
    let locale_ctx = LocaleContext::from_locale_string(None, &state.config.locale);

    if let Some(ref f) = file
        && !f.data.is_empty()
        && let Ok(conn) = state.pool.get()
        && let Ok(Some(old_doc)) = query::find_by_id(&conn, &slug, &def, &id, locale_ctx.as_ref())
    {
        old_doc_fields = Some(old_doc.fields.clone());
    }

    let mut queued_conversions = Vec::new();
    let mut upload_guard: Option<CleanupGuard> = None;

    if let Some(f) = file
        && let Some(upload_config) = def.upload.clone()
    {
        let storage = state.storage.clone();
        let slug_for_upload = slug.clone();
        let global_max = state.config.upload.max_file_size;

        match task::spawn_blocking(move || {
            process_upload(f, &upload_config, storage, &slug_for_upload, global_max)
        })
        .await
        {
            Ok(Ok((processed, guard))) => {
                queued_conversions = processed.queued_conversions.clone();
                upload_guard = Some(guard);
                inject_upload_metadata(&mut form_data, &processed);
            }
            Ok(Err(e)) => return json_error(StatusCode::BAD_REQUEST, &e.to_string()),
            Err(e) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Task error: {}", e),
                );
            }
        }
    }

    strip_write_denied_fields(&state, &def.fields, user_doc, "update", &mut form_data);

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
    let id_owned = id.clone();
    let def_owned = def.clone();
    let user_doc_owned = auth_user.as_ref().map(|au| au.user_doc.clone());
    let ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());

    let result = task::spawn_blocking(move || {
        update_document(
            &pool,
            &runner,
            &slug_owned,
            &id_owned,
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
            if let Some(mut g) = upload_guard {
                g.commit();
            }

            if let Some(old_fields) = old_doc_fields {
                delete_upload_files(&*state.storage, &old_fields);
            }

            if !queued_conversions.is_empty()
                && let Ok(conn) = state.pool.get()
                && let Err(e) = enqueue_conversions(&conn, &slug, &id, &queued_conversions)
            {
                warn!("Failed to enqueue image conversions: {}", e);
            }

            strip_read_denied_doc_fields(&state, &def.fields, &auth_user, &mut doc.fields);

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
        Ok(Err(e)) => json_error(StatusCode::BAD_REQUEST, &e.to_string()),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task error: {}", e),
        ),
    }
}
