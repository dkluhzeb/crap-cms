pub(super) use super::list_helpers::{
    build_column_options, build_filter_fields, build_filter_pills, compute_cells, resolve_columns,
};
use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
        handlers::{
            collections::forms::{extract_join_data_from_form, transform_select_has_many},
            shared::{
                EnrichOptions, apply_display_conditions, build_field_contexts,
                check_access_or_forbid, enrich_field_contexts, forbidden, get_event_user,
                get_user_doc, html_with_toast, htmx_redirect, redirect_response,
                split_sidebar_fields, strip_write_denied_string_fields,
                translate_validation_errors,
            },
        },
    },
    core::{
        auth::AuthUser,
        collection::CollectionDefinition,
        event::{EventOperation, EventTarget},
        field::FieldDefinition,
        upload::{
            CleanupGuard, UploadedFile, delete_upload_files, enqueue_conversions,
            inject_upload_metadata, process_upload,
        },
        validate::ValidationError,
    },
    db::query::{self, AccessResult, LocaleContext, LocaleMode},
    hooks::lifecycle::PublishEventInput,
    service,
};

use anyhow::Context;
use axum::{
    Extension,
    response::{IntoResponse, Redirect, Response},
};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use tokio::task;

/// Render the upload error page (create form with toast).
pub(super) fn render_upload_error(
    state: &AdminState,
    def: &CollectionDefinition,
    form_data: &HashMap<String, String>,
    auth_user: &Option<Extension<AuthUser>>,
    err_msg: &str,
) -> Response {
    let mut fields = build_field_contexts(&def.fields, form_data, &HashMap::new(), true, false);

    enrich_field_contexts(
        &mut fields,
        &def.fields,
        &HashMap::new(),
        state,
        &EnrichOptions::builder(&HashMap::new())
            .filter_hidden(true)
            .build(),
    );

    let empty_data = json!({});

    apply_display_conditions(
        &mut fields,
        &def.fields,
        &empty_data,
        &state.hook_runner,
        true,
    );

    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    let data = ContextBuilder::new(state, None)
        .locale_from_auth(auth_user)
        .page(PageType::CollectionCreate, "create_name")
        .page_title_name(def.singular_name())
        .collection_def(def)
        .fields(main_fields)
        .set("sidebar_fields", json!(sidebar_fields))
        .set("editing", json!(false))
        .set("has_drafts", json!(def.has_drafts()))
        .build();

    html_with_toast(state, "collections/edit", &data, err_msg)
}

/// Collect hidden upload field values from form data for re-rendering after validation errors.
pub(super) fn collect_upload_hidden_fields(
    fields: &[FieldDefinition],
    form_data: &HashMap<String, String>,
) -> Value {
    let hidden_fields: Vec<Value> = fields
        .iter()
        .filter(|f| f.admin.hidden)
        .filter_map(|f| {
            form_data
                .get(&f.name)
                .map(|v| json!({"name": &f.name, "value": v}))
        })
        .collect();

    json!(hidden_fields)
}

/// Render the upload error page (edit form with toast).
pub(super) fn render_edit_upload_error(
    state: &AdminState,
    def: &CollectionDefinition,
    form_data: &HashMap<String, String>,
    id: &str,
    auth_user: &Option<Extension<AuthUser>>,
    err_msg: &str,
) -> Response {
    let mut fields = build_field_contexts(&def.fields, form_data, &HashMap::new(), true, false);

    enrich_field_contexts(
        &mut fields,
        &def.fields,
        &HashMap::new(),
        state,
        &EnrichOptions::builder(&HashMap::new())
            .filter_hidden(true)
            .doc_id(id)
            .build(),
    );

    let form_json = json!(
        form_data
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect::<Map<String, Value>>()
    );

    apply_display_conditions(
        &mut fields,
        &def.fields,
        &form_json,
        &state.hook_runner,
        true,
    );

    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    let data = ContextBuilder::new(state, None)
        .locale_from_auth(auth_user)
        .page(PageType::CollectionEdit, "edit_name")
        .page_title_name(def.singular_name())
        .collection_def(def)
        .document_stub(id)
        .fields(main_fields)
        .set("sidebar_fields", json!(sidebar_fields))
        .set("editing", json!(true))
        .set("has_drafts", json!(def.has_drafts()))
        .build();

    html_with_toast(state, "collections/edit", &data, err_msg)
}

pub(super) async fn do_update(
    state: &AdminState,
    slug: &str,
    id: &str,
    mut form_data: HashMap<String, String>,
    file: Option<UploadedFile>,
    auth_user: &Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_collection(slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin/collections").into_response(),
    };

    // Extract action (publish/save_draft/unpublish) and locale
    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let form_locale = form_data.remove("_locale");
    let locale_ctx =
        LocaleContext::from_locale_string(form_locale.as_deref(), &state.config.locale);

    // Check update access
    match check_access_or_forbid(
        state,
        def.access.update.as_deref(),
        auth_user,
        Some(id),
        None,
    ) {
        Ok(AccessResult::Denied) => {
            return forbidden(state, "You don't have permission to update this item")
                .into_response();
        }
        Err(resp) => return *resp,
        _ => {}
    }

    // For upload collections, if a new file was uploaded, process it and delete old files
    let mut old_doc_fields: Option<HashMap<String, Value>> = None;
    let mut queued_conversions = Vec::new();
    let mut upload_guard: Option<CleanupGuard> = None;

    if let Some(f) = file
        && let Some(upload_config) = def.upload.clone()
    {
        // Load old document to get old file paths for cleanup
        if let Ok(conn) = state.pool.get()
            && let Ok(Some(old_doc)) = query::find_by_id(&conn, slug, &def, id, locale_ctx.as_ref())
        {
            old_doc_fields = Some(old_doc.fields.clone());
        }

        let config_dir = state.config_dir.clone();
        let slug_for_upload = slug.to_string();
        let global_max = state.config.upload.max_file_size;

        let upload_result = task::spawn_blocking(move || {
            process_upload(f, &upload_config, &config_dir, &slug_for_upload, global_max)
        })
        .await;

        match upload_result {
            Ok(Ok((processed, guard))) => {
                queued_conversions = processed.queued_conversions.clone();
                upload_guard = Some(guard);
                inject_upload_metadata(&mut form_data, &processed);
            }
            Ok(Err(e)) => {
                tracing::error!("Upload processing error: {}", e);
                return render_edit_upload_error(
                    state,
                    &def,
                    &form_data,
                    id,
                    auth_user,
                    &e.to_string(),
                )
                .into_response();
            }
            Err(e) => {
                tracing::error!("Upload task error: {}", e);
                return render_edit_upload_error(
                    state,
                    &def,
                    &form_data,
                    id,
                    auth_user,
                    &e.to_string(),
                )
                .into_response();
            }
        }
    }

    // Strip field-level update-denied fields (fail closed on pool exhaustion)
    if let Err(resp) =
        strip_write_denied_string_fields(state, auth_user, &def.fields, "update", &mut form_data)
    {
        return (*resp).into_response();
    }

    // Extract password and locked before they enter hooks/regular data flow
    let password = if def.is_auth_collection() {
        form_data.remove("password")
    } else {
        None
    };
    let locked_value = if def.is_auth_collection() {
        Some(form_data.remove("_locked"))
    } else {
        None
    };

    // Validate password against policy (update: empty password means "keep current")
    if let Some(ref pw) = password
        && !pw.is_empty()
        && let Err(e) = state.config.auth.password_policy.validate(pw)
    {
        return html_with_toast(state, "collections/edit_form", &json!({}), &e.to_string())
            .into_response();
    }

    // Convert comma-separated multi-select values to JSON arrays
    transform_select_has_many(&mut form_data, &def.fields);

    // Extract join table data (arrays + has-many relationships) from form
    let join_data = extract_join_data_from_form(&form_data, &def.fields);

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.to_string();
    let id_owned = id.to_string();
    let def_owned = def.clone();
    let form_data_clone = form_data.clone();
    let join_data_clone = join_data.clone();
    let user_doc = get_user_doc(auth_user).cloned();
    let locale = locale_ctx.as_ref().and_then(|ctx| match &ctx.mode {
        LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });
    let ui_locale = auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone());
    let action_owned = action.clone();

    let result = task::spawn_blocking(move || {
        // Handle unpublish: set _status to 'draft' and create a version
        let result = if action_owned == "unpublish" && def_owned.has_versions() {
            let doc = service::unpublish_document(
                &pool,
                &runner,
                &slug_owned,
                &id_owned,
                &def_owned,
                user_doc.as_ref(),
            )?;

            Ok((doc, HashMap::new()))
        } else {
            service::update_document(
                &pool,
                &runner,
                &slug_owned,
                &id_owned,
                &def_owned,
                service::WriteInput::builder(form_data, &join_data)
                    .password(password.as_deref())
                    .locale_ctx(locale_ctx.as_ref())
                    .locale(locale)
                    .draft(draft)
                    .ui_locale(ui_locale)
                    .build(),
                user_doc.as_ref(),
            )
        };

        // Update lock status for auth collections (after successful update)
        if result.is_ok()
            && let Some(locked_field) = locked_value
        {
            let should_lock =
                locked_field.as_deref() == Some("on") || locked_field.as_deref() == Some("1");
            let conn = pool.get().context("DB connection for lock update")?;
            if should_lock {
                query::auth::lock_user(&conn, &slug_owned, &id_owned)?;
            } else {
                query::auth::unlock_user(&conn, &slug_owned, &id_owned)?;
            }
        }

        result
    })
    .await;

    match result {
        Ok(Ok((doc, _req_context))) => {
            if let Some(mut g) = upload_guard {
                g.commit();
            }

            // If a new file was uploaded and old files exist, clean up old files
            if let Some(old_fields) = old_doc_fields {
                delete_upload_files(&state.config_dir, &old_fields);
            }

            // Enqueue deferred image conversions if any
            if !queued_conversions.is_empty()
                && let Ok(conn) = state.pool.get()
                && let Err(e) = enqueue_conversions(&conn, slug, id, &queued_conversions)
            {
                tracing::warn!("Failed to enqueue image conversions: {}", e);
            }

            state.hook_runner.publish_event(
                &state.event_bus,
                &def.hooks,
                def.live.as_ref(),
                PublishEventInput::builder(EventTarget::Collection, EventOperation::Update)
                    .collection(slug.to_string())
                    .document_id(id.to_string())
                    .data(doc.fields.clone())
                    .edited_by(get_event_user(auth_user))
                    .build(),
            );

            htmx_redirect(&format!("/admin/collections/{}/{}", slug, id))
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let locale = auth_user
                    .as_ref()
                    .map(|Extension(au)| au.ui_locale.as_str())
                    .unwrap_or("en");
                let error_map = translate_validation_errors(ve, &state.translations, locale);
                let toast_msg = state.translations.get(locale, "validation.error_summary");
                let mut fields =
                    build_field_contexts(&def.fields, &form_data_clone, &error_map, true, false);

                enrich_field_contexts(
                    &mut fields,
                    &def.fields,
                    &join_data_clone,
                    state,
                    &EnrichOptions::builder(&error_map)
                        .filter_hidden(true)
                        .doc_id(id)
                        .build(),
                );

                let form_json = json!(
                    form_data_clone
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                        .collect::<Map<String, Value>>()
                );

                apply_display_conditions(
                    &mut fields,
                    &def.fields,
                    &form_json,
                    &state.hook_runner,
                    true,
                );

                let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

                let mut data = ContextBuilder::new(state, None)
                    .locale_from_auth(auth_user)
                    .page(PageType::CollectionEdit, "edit_name")
                    .page_title_name(def.singular_name())
                    .collection_def(&def)
                    .document_stub(id)
                    .fields(main_fields)
                    .set("sidebar_fields", json!(sidebar_fields))
                    .set("editing", json!(true))
                    .set("has_drafts", json!(def.has_drafts()))
                    .build();

                // Preserve upload metadata as hidden inputs so they survive form re-submission
                if def.is_upload_collection() {
                    data["upload_hidden_fields"] =
                        collect_upload_hidden_fields(&def.fields, &form_data_clone);
                }

                html_with_toast(state, "collections/edit", &data, toast_msg).into_response()
            } else {
                tracing::error!("Update error: {}", e);
                redirect_response(&format!("/admin/collections/{}/{}", slug, id))
            }
        }
        Err(e) => {
            tracing::error!("Update task error: {}", e);
            redirect_response(&format!("/admin/collections/{}/{}", slug, id))
        }
    }
}

pub(super) async fn delete_action_impl(
    state: &AdminState,
    slug: &str,
    id: &str,
    auth_user: &Option<Extension<AuthUser>>,
    force_hard_delete: bool,
    json_response: bool,
) -> axum::response::Response {
    let def = match state.registry.get_collection(slug) {
        Some(d) => d.clone(),
        None => {
            if json_response {
                return json_error_response("Collection not found");
            }
            return Redirect::to("/admin/collections").into_response();
        }
    };

    // Permission check depends on the type of deletion:
    // - Soft delete (trash): check access.trash, falling back to access.update
    // - Permanent delete: check access.delete (explicit permission required)
    if def.soft_delete && !force_hard_delete {
        // Soft delete / move to trash
        let trash_access = def.access.resolve_trash();

        match check_access_or_forbid(state, trash_access, auth_user, Some(id), None) {
            Ok(AccessResult::Denied) => {
                let msg = "You don't have permission to trash this item";
                if json_response {
                    return json_error_response(msg);
                }
                return forbidden(state, msg).into_response();
            }
            Err(resp) => return *resp,
            _ => {}
        }
    } else {
        // Permanent deletion (no soft_delete, or force_hard_delete)
        match check_access_or_forbid(
            state,
            def.access.delete.as_deref(),
            auth_user,
            Some(id),
            None,
        ) {
            Ok(AccessResult::Denied) => {
                let msg = "You don't have permission to permanently delete this item";
                if json_response {
                    return json_error_response(msg);
                }
                return forbidden(state, msg).into_response();
            }
            Err(resp) => return *resp,
            _ => {}
        }
    }

    // Before hooks + delete + upload cleanup in a single transaction
    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let mut def_clone = def.clone();
    let slug_owned = slug.to_string();
    let id_owned = id.to_string();
    let user_doc = get_user_doc(auth_user).cloned();
    let config_dir = state.config_dir.clone();

    // When force_hard_delete is requested, temporarily override soft_delete
    if force_hard_delete {
        def_clone.soft_delete = false;
    }

    let locale_config = state.config.locale.clone();

    let result = task::spawn_blocking(move || {
        service::delete_document(
            &pool,
            &runner,
            &slug_owned,
            &id_owned,
            &def_clone,
            user_doc.as_ref(),
            Some(&config_dir),
            Some(&locale_config),
        )
    })
    .await;

    match result {
        Ok(Ok(_req_context)) => {
            state.hook_runner.publish_event(
                &state.event_bus,
                &def.hooks,
                def.live.as_ref(),
                PublishEventInput::builder(EventTarget::Collection, EventOperation::Delete)
                    .collection(slug.to_string())
                    .document_id(id.to_string())
                    .edited_by(get_event_user(auth_user))
                    .build(),
            );

            if json_response {
                return json_ok_response();
            }
        }
        Ok(Err(e)) => {
            tracing::error!("Delete error: {}", e);

            if json_response {
                return json_error_response("Failed to delete item");
            }
        }
        Err(e) => {
            tracing::error!("Delete task error: {}", e);

            if json_response {
                return json_error_response("Failed to delete item");
            }
        }
    }

    htmx_redirect(&format!("/admin/collections/{}", slug))
}

/// Build a JSON `{"ok": true}` success response.
fn json_ok_response() -> Response {
    axum::response::Json(json!({"ok": true})).into_response()
}

/// Build a JSON `{"error": "..."}` error response with 400 status.
fn json_error_response(msg: &str) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::response::Json(json!({"error": msg})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldAdmin, FieldDefinition, FieldType};

    #[test]
    fn collect_upload_hidden_fields_basic() {
        let fields = vec![
            FieldDefinition::builder("filename", FieldType::Text).build(),
            FieldDefinition::builder("mime_type", FieldType::Text)
                .admin(
                    crate::core::field::FieldAdmin::builder()
                        .hidden(true)
                        .build(),
                )
                .build(),
            FieldDefinition::builder("url", FieldType::Text)
                .admin(
                    crate::core::field::FieldAdmin::builder()
                        .hidden(true)
                        .build(),
                )
                .build(),
            FieldDefinition::builder("width", FieldType::Number)
                .admin(
                    crate::core::field::FieldAdmin::builder()
                        .hidden(true)
                        .build(),
                )
                .build(),
            FieldDefinition::builder("alt", FieldType::Text).build(),
        ];
        let mut form_data = HashMap::new();
        form_data.insert("filename".to_string(), "test.jpg".to_string());
        form_data.insert("mime_type".to_string(), "image/jpeg".to_string());
        form_data.insert("url".to_string(), "/uploads/media/test.jpg".to_string());
        form_data.insert("width".to_string(), "1920".to_string());
        form_data.insert("alt".to_string(), "Test".to_string());

        let result = collect_upload_hidden_fields(&fields, &form_data);
        let arr = result.as_array().unwrap();

        assert_eq!(arr.len(), 3);
        assert!(
            arr.iter()
                .any(|f| f["name"] == "mime_type" && f["value"] == "image/jpeg")
        );
        assert!(
            arr.iter()
                .any(|f| f["name"] == "url" && f["value"] == "/uploads/media/test.jpg")
        );
        assert!(
            arr.iter()
                .any(|f| f["name"] == "width" && f["value"] == "1920")
        );
    }

    #[test]
    fn collect_upload_hidden_fields_missing_values() {
        let fields = vec![
            FieldDefinition::builder("url", FieldType::Text)
                .admin(FieldAdmin::builder().hidden(true).build())
                .build(),
            FieldDefinition::builder("mime_type", FieldType::Text)
                .admin(FieldAdmin::builder().hidden(true).build())
                .build(),
        ];
        // Only url is in form_data, not mime_type
        let mut form_data = HashMap::new();
        form_data.insert("url".to_string(), "/uploads/media/test.jpg".to_string());

        let result = collect_upload_hidden_fields(&fields, &form_data);
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "url");
    }

    #[test]
    fn collect_upload_hidden_fields_no_hidden() {
        let fields = vec![FieldDefinition::builder("alt", FieldType::Text).build()];
        let form_data = HashMap::new();
        let result = collect_upload_hidden_fields(&fields, &form_data);
        assert_eq!(result.as_array().unwrap().len(), 0);
    }

    #[test]
    fn json_ok_response_returns_200() {
        let resp = json_ok_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }

    #[test]
    fn json_error_response_returns_400() {
        let resp = json_error_response("something went wrong");
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn trash_access_falls_back_to_update() {
        use crate::core::collection::Access;

        // When trash is set, it takes priority
        let access = Access {
            trash: Some("access.trash_fn".to_string()),
            update: Some("access.update_fn".to_string()),
            ..Default::default()
        };
        assert_eq!(access.resolve_trash(), Some("access.trash_fn"));

        // When trash is None, falls back to update
        let access = Access {
            trash: None,
            update: Some("access.update_fn".to_string()),
            ..Default::default()
        };
        assert_eq!(access.resolve_trash(), Some("access.update_fn"));

        // When both are None, resolved is None
        let access = Access::default();
        assert!(access.resolve_trash().is_none());
    }
}
