use crate::admin::context::{ContextBuilder, PageType};
use crate::admin::handlers::collections::forms::{
    extract_join_data_from_form, transform_select_has_many,
};
use crate::admin::handlers::shared::{
    apply_display_conditions, auto_label_from_name, build_field_contexts, build_list_url,
    check_access_or_forbid, enrich_field_contexts, forbidden, get_event_user, get_user_doc,
    html_with_toast, htmx_redirect, is_column_eligible, redirect_response, server_error,
    split_sidebar_fields, translate_validation_errors, url_decode,
};
use crate::admin::AdminState;
use crate::core::auth::AuthUser;
use crate::core::collection::CollectionDefinition;
use crate::core::document::Document;
use crate::core::event::{EventOperation, EventTarget};
use crate::core::field::{FieldDefinition, FieldType};
use crate::core::upload::{
    delete_upload_files, enqueue_conversions, inject_upload_metadata, process_upload, UploadedFile,
};
use crate::core::validate::ValidationError;
use crate::db::query::{self, AccessResult, FilterClause, FilterOp, LocaleContext, LocaleMode};
use crate::service;
use anyhow::Context;
use axum::response::IntoResponse;
use axum::Extension;
use std::collections::HashMap;

/// Get the display label for a field (admin label or auto-generated from name).
pub(super) fn field_label(field: &FieldDefinition) -> String {
    if let Some(ref label) = field.admin.label {
        label.resolve_default().to_string()
    } else {
        auto_label_from_name(&field.name)
    }
}

/// Resolve which columns to display in the list table.
pub(super) fn resolve_columns(
    def: &CollectionDefinition,
    user_cols: Option<&[String]>,
    sort: Option<&str>,
    base_url: &str,
    raw_where: &str,
    search: Option<&str>,
) -> Vec<serde_json::Value> {
    let mut keys: Vec<String> = if let Some(cols) = user_cols {
        cols.iter()
            .filter(|k| {
                k.as_str() == "created_at"
                    || k.as_str() == "updated_at"
                    || k.as_str() == "_status"
                    || def
                        .fields
                        .iter()
                        .any(|f| f.name == **k && is_column_eligible(&f.field_type))
            })
            .cloned()
            .collect()
    } else {
        let mut defaults = Vec::new();
        if def.has_drafts() {
            defaults.push("_status".to_string());
        }
        defaults.push("created_at".to_string());
        defaults
    };
    if let Some(title) = def.title_field() {
        keys.retain(|k| k != title);
    }

    let sort_field = sort.map(|s| s.strip_prefix('-').unwrap_or(s));
    let sort_desc = sort.map(|s| s.starts_with('-')).unwrap_or(false);

    keys.iter()
        .map(|key| {
            let (label, sortable) = match key.as_str() {
                "created_at" => ("Created".to_string(), true),
                "updated_at" => ("Updated".to_string(), true),
                "_status" => ("Status".to_string(), true),
                _ => {
                    if let Some(f) = def.fields.iter().find(|f| f.name == *key) {
                        (field_label(f), true)
                    } else {
                        (auto_label_from_name(key), false)
                    }
                }
            };

            let is_sorted = sort_field == Some(key.as_str());
            let next_sort = if is_sorted && !sort_desc {
                format!("-{}", key)
            } else {
                key.clone()
            };
            let sort_url = build_list_url(base_url, 1, None, search, Some(&next_sort), raw_where);

            serde_json::json!({
                "key": key,
                "label": label,
                "sortable": sortable,
                "sort_url": sort_url,
                "is_sorted_asc": is_sorted && !sort_desc,
                "is_sorted_desc": is_sorted && sort_desc,
            })
        })
        .collect()
}

/// Pre-compute cell values for a document row, parallel to the columns array.
pub(super) fn compute_cells(
    doc: &Document,
    columns: &[serde_json::Value],
    def: &CollectionDefinition,
) -> Vec<serde_json::Value> {
    columns
        .iter()
        .map(|col| {
            let key = col["key"].as_str().unwrap_or("");
            match key {
                "_status" => {
                    let status = doc
                        .fields
                        .get("_status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("published");
                    serde_json::json!({ "value": status, "is_badge": true })
                }
                "created_at" => {
                    serde_json::json!({ "value": doc.created_at, "is_date": true })
                }
                "updated_at" => {
                    serde_json::json!({ "value": doc.updated_at, "is_date": true })
                }
                _ => {
                    let field_def = def.fields.iter().find(|f| f.name == key);
                    let raw = doc
                        .fields
                        .get(key)
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);

                    if let Some(f) = field_def {
                        match f.field_type {
                            FieldType::Checkbox => {
                                let checked = match &raw {
                                    serde_json::Value::Bool(b) => *b,
                                    serde_json::Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
                                    _ => false,
                                };
                                serde_json::json!({ "value": checked, "is_bool": true })
                            }
                            FieldType::Date => {
                                let val = raw.as_str().unwrap_or("");
                                serde_json::json!({ "value": val, "is_date": true })
                            }
                            FieldType::Select | FieldType::Radio => {
                                let raw_val = raw.as_str().unwrap_or("");
                                let label = f
                                    .options
                                    .iter()
                                    .find(|o| o.value == raw_val)
                                    .map(|o| o.label.resolve_default().to_string())
                                    .unwrap_or_else(|| raw_val.to_string());
                                serde_json::json!({ "value": label })
                            }
                            FieldType::Textarea => {
                                let text = raw.as_str().unwrap_or("");
                                let truncated = if text.len() > 80 {
                                    format!("{}…", &text[..80])
                                } else {
                                    text.to_string()
                                };
                                serde_json::json!({ "value": truncated })
                            }
                            _ => {
                                let val = match &raw {
                                    serde_json::Value::String(s) => s.clone(),
                                    serde_json::Value::Number(n) => n.to_string(),
                                    serde_json::Value::Bool(b) => b.to_string(),
                                    serde_json::Value::Null => String::new(),
                                    other => other.to_string(),
                                };
                                serde_json::json!({ "value": val })
                            }
                        }
                    } else {
                        let val = match &raw {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Null => String::new(),
                            other => other.to_string(),
                        };
                        serde_json::json!({ "value": val })
                    }
                }
            }
        })
        .collect()
}

/// Build the list of all eligible columns for the column picker UI.
pub(super) fn build_column_options(
    def: &CollectionDefinition,
    selected_keys: &[String],
) -> Vec<serde_json::Value> {
    let mut options = Vec::new();

    if def.has_drafts() {
        options.push(serde_json::json!({
            "key": "_status",
            "label": "Status",
            "selected": selected_keys.contains(&"_status".to_string()),
        }));
    }
    options.push(serde_json::json!({
        "key": "created_at",
        "label": "Created",
        "selected": selected_keys.contains(&"created_at".to_string()),
    }));
    options.push(serde_json::json!({
        "key": "updated_at",
        "label": "Updated",
        "selected": selected_keys.contains(&"updated_at".to_string()),
    }));

    let title_field = def.title_field();
    for f in &def.fields {
        if Some(f.name.as_str()) == title_field {
            continue;
        }
        if is_column_eligible(&f.field_type) {
            options.push(serde_json::json!({
                "key": f.name,
                "label": field_label(f),
                "selected": selected_keys.contains(&f.name),
            }));
        }
    }

    options
}

/// Build filter field metadata for the filter builder UI.
pub(super) fn build_filter_fields(def: &CollectionDefinition) -> Vec<serde_json::Value> {
    let mut fields = Vec::new();

    if def.has_drafts() {
        fields.push(serde_json::json!({
            "key": "_status",
            "label": "Status",
            "field_type": "select",
            "options": [
                { "label": "Published", "value": "published" },
                { "label": "Draft", "value": "draft" },
            ],
        }));
    }
    fields.push(serde_json::json!({
        "key": "created_at",
        "label": "Created",
        "field_type": "date",
    }));
    fields.push(serde_json::json!({
        "key": "updated_at",
        "label": "Updated",
        "field_type": "date",
    }));

    for f in &def.fields {
        if !is_column_eligible(&f.field_type) {
            continue;
        }
        let ft = format!("{:?}", f.field_type).to_lowercase();
        let mut field_info = serde_json::json!({
            "key": f.name,
            "label": field_label(f),
            "field_type": ft,
        });
        if !f.options.is_empty() {
            let opts: Vec<serde_json::Value> = f
                .options
                .iter()
                .map(|o| {
                    serde_json::json!({
                        "label": o.label.resolve_default(),
                        "value": o.value,
                    })
                })
                .collect();
            field_info["options"] = serde_json::json!(opts);
        }
        fields.push(field_info);
    }

    fields
}

/// Build active filter pills from parsed filter clauses.
pub(super) fn build_filter_pills(
    parsed: &[FilterClause],
    def: &CollectionDefinition,
    raw_query: &str,
) -> Vec<serde_json::Value> {
    parsed
        .iter()
        .filter_map(|clause| {
            let FilterClause::Single(filter) = clause else {
                return None;
            };
            let field_label_str = match filter.field.as_str() {
                "created_at" => "Created".to_string(),
                "updated_at" => "Updated".to_string(),
                "_status" => "Status".to_string(),
                name => def
                    .fields
                    .iter()
                    .find(|f| f.name == name)
                    .map(field_label)
                    .unwrap_or_else(|| auto_label_from_name(name)),
            };

            let (op_label, value) = match &filter.op {
                FilterOp::Equals(v) => ("is", v.clone()),
                FilterOp::NotEquals(v) => ("is not", v.clone()),
                FilterOp::Contains(v) => ("contains", v.clone()),
                FilterOp::Like(v) => ("like", v.clone()),
                FilterOp::GreaterThan(v) => (">", v.clone()),
                FilterOp::LessThan(v) => ("<", v.clone()),
                FilterOp::GreaterThanOrEqual(v) => (">=", v.clone()),
                FilterOp::LessThanOrEqual(v) => ("<=", v.clone()),
                FilterOp::Exists => ("exists", String::new()),
                FilterOp::NotExists => ("not exists", String::new()),
                _ => return None,
            };

            let filter_key = format!("where[{}][{}]", filter.field, op_to_param_name(&filter.op));
            let remove_query: Vec<&str> = raw_query
                .split('&')
                .filter(|p| {
                    let decoded = url_decode(p.split('=').next().unwrap_or(""));
                    decoded != filter_key
                })
                .collect();
            let remove_url = if remove_query.is_empty() {
                String::new()
            } else {
                format!("?{}", remove_query.join("&"))
            };

            Some(serde_json::json!({
                "field_label": field_label_str,
                "op": op_label,
                "value": value,
                "remove_url": remove_url,
            }))
        })
        .collect()
}

/// Delete a list of files, ignoring errors (best-effort orphan cleanup).
pub(super) fn cleanup_created_files(files: &[std::path::PathBuf]) {
    for path in files {
        let _ = std::fs::remove_file(path);
    }
}

/// Render the upload error page (create form with toast).
pub(super) fn render_upload_error(
    state: &AdminState,
    def: &CollectionDefinition,
    form_data: &HashMap<String, String>,
    auth_user: &Option<Extension<AuthUser>>,
    err_msg: &str,
) -> axum::response::Response {
    let mut fields = build_field_contexts(&def.fields, form_data, &HashMap::new(), true, false);
    enrich_field_contexts(
        &mut fields,
        &def.fields,
        &HashMap::new(),
        state,
        true,
        false,
        &HashMap::new(),
        None,
    );
    let empty_data = serde_json::json!({});
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
        .page(
            PageType::CollectionCreate,
            format!("Create {}", def.singular_name()),
        )
        .set(
            "page_title",
            serde_json::json!(format!("Create {}", def.singular_name())),
        )
        .collection_def(def)
        .fields(main_fields)
        .set("sidebar_fields", serde_json::json!(sidebar_fields))
        .set("editing", serde_json::json!(false))
        .set("has_drafts", serde_json::json!(def.has_drafts()))
        .build();
    html_with_toast(state, "collections/edit", &data, err_msg)
}

/// Collect hidden upload field values from form data for re-rendering after validation errors.
pub(super) fn collect_upload_hidden_fields(
    fields: &[crate::core::field::FieldDefinition],
    form_data: &HashMap<String, String>,
) -> serde_json::Value {
    let hidden_fields: Vec<serde_json::Value> = fields
        .iter()
        .filter(|f| f.admin.hidden)
        .filter_map(|f| {
            form_data
                .get(&f.name)
                .map(|v| serde_json::json!({"name": &f.name, "value": v}))
        })
        .collect();
    serde_json::json!(hidden_fields)
}

/// Render the upload error page (edit form with toast).
pub(super) fn render_edit_upload_error(
    state: &AdminState,
    def: &CollectionDefinition,
    form_data: &HashMap<String, String>,
    id: &str,
    auth_user: &Option<Extension<AuthUser>>,
    err_msg: &str,
) -> axum::response::Response {
    let mut fields = build_field_contexts(&def.fields, form_data, &HashMap::new(), true, false);
    enrich_field_contexts(
        &mut fields,
        &def.fields,
        &HashMap::new(),
        state,
        true,
        false,
        &HashMap::new(),
        Some(id),
    );
    let form_json = serde_json::json!(form_data
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect::<serde_json::Map<String, serde_json::Value>>());
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
        .page(
            PageType::CollectionEdit,
            format!("Edit {}", def.singular_name()),
        )
        .set(
            "page_title",
            serde_json::json!(format!("Edit {}", def.singular_name())),
        )
        .collection_def(def)
        .document_stub(id)
        .fields(main_fields)
        .set("sidebar_fields", serde_json::json!(sidebar_fields))
        .set("editing", serde_json::json!(true))
        .set("has_drafts", serde_json::json!(def.has_drafts()))
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
) -> axum::response::Response {
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
                .into_response()
        }
        Err(resp) => return resp,
        _ => {}
    }

    // For upload collections, if a new file was uploaded, process it and delete old files
    let mut old_doc_fields: Option<HashMap<String, serde_json::Value>> = None;
    let mut queued_conversions = Vec::new();
    let mut created_files: Vec<std::path::PathBuf> = Vec::new();
    if let Some(f) = file {
        if let Some(upload_config) = def.upload.clone() {
            // Load old document to get old file paths for cleanup
            if let Ok(conn) = state.pool.get() {
                if let Ok(Some(old_doc)) =
                    query::find_by_id(&conn, slug, &def, id, locale_ctx.as_ref())
                {
                    old_doc_fields = Some(old_doc.fields.clone());
                }
            }

            let config_dir = state.config_dir.clone();
            let slug_for_upload = slug.to_string();
            let global_max = state.config.upload.max_file_size;
            let upload_result = tokio::task::spawn_blocking(move || {
                process_upload(f, &upload_config, &config_dir, &slug_for_upload, global_max)
            })
            .await;
            match upload_result {
                Ok(Ok(processed)) => {
                    queued_conversions = processed.queued_conversions.clone();
                    created_files = processed.created_files.clone();
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
    }

    // Strip field-level update-denied fields (fail closed on pool exhaustion)
    if def.fields.iter().any(|f| f.access.update.is_some()) {
        let user_doc = get_user_doc(auth_user);
        let mut conn = match state.pool.get() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Field access check pool error: {}", e);
                return server_error(state, "Database error").into_response();
            }
        };
        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("Field access check tx error: {}", e);
                return server_error(state, "Database error").into_response();
            }
        };
        let denied =
            state
                .hook_runner
                .check_field_write_access(&def.fields, user_doc, "update", &tx);
        // Read-only access check — commit result is irrelevant, rollback on drop is safe
        let _ = tx.commit();
        for name in &denied {
            form_data.remove(name);
        }
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
    if let Some(ref pw) = password {
        if !pw.is_empty() {
            if let Err(e) = state.config.auth.password_policy.validate(pw) {
                return html_with_toast(
                    state,
                    "collections/edit_form",
                    &serde_json::json!({}),
                    &e.to_string(),
                )
                .into_response();
            }
        }
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
    let result = tokio::task::spawn_blocking(move || {
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
                service::WriteInput {
                    data: form_data,
                    join_data: &join_data,
                    password: password.as_deref(),
                    locale_ctx: locale_ctx.as_ref(),
                    locale,
                    draft,
                    ui_locale,
                },
                user_doc.as_ref(),
            )
        };

        // Update lock status for auth collections (after successful update)
        if result.is_ok() {
            if let Some(locked_field) = locked_value {
                let should_lock =
                    locked_field.as_deref() == Some("on") || locked_field.as_deref() == Some("1");
                let conn = pool.get().context("DB connection for lock update")?;
                if should_lock {
                    query::auth::lock_user(&conn, &slug_owned, &id_owned)?;
                } else {
                    query::auth::unlock_user(&conn, &slug_owned, &id_owned)?;
                }
            }
        }

        result
    })
    .await;

    match result {
        Ok(Ok((doc, _req_context))) => {
            // If a new file was uploaded and old files exist, clean up old files
            if let Some(old_fields) = old_doc_fields {
                delete_upload_files(&state.config_dir, &old_fields);
            }

            // Enqueue deferred image conversions if any
            if !queued_conversions.is_empty() {
                if let Ok(conn) = state.pool.get() {
                    if let Err(e) = enqueue_conversions(&conn, slug, id, &queued_conversions) {
                        tracing::warn!("Failed to enqueue image conversions: {}", e);
                    }
                }
            }

            state.hook_runner.publish_event(
                &state.event_bus,
                &def.hooks,
                def.live.as_ref(),
                EventTarget::Collection,
                EventOperation::Update,
                slug.to_string(),
                id.to_string(),
                doc.fields.clone(),
                get_event_user(auth_user),
            );
            htmx_redirect(&format!("/admin/collections/{}/{}", slug, id))
        }
        Ok(Err(e)) => {
            cleanup_created_files(&created_files);
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
                    true,
                    false,
                    &error_map,
                    Some(id),
                );
                let form_json = serde_json::json!(form_data_clone
                    .iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect::<serde_json::Map<String, serde_json::Value>>());
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
                    .page(
                        PageType::CollectionEdit,
                        format!("Edit {}", def.singular_name()),
                    )
                    .set(
                        "page_title",
                        serde_json::json!(format!("Edit {}", def.singular_name())),
                    )
                    .collection_def(&def)
                    .document_stub(id)
                    .fields(main_fields)
                    .set("sidebar_fields", serde_json::json!(sidebar_fields))
                    .set("editing", serde_json::json!(true))
                    .set("has_drafts", serde_json::json!(def.has_drafts()))
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
            cleanup_created_files(&created_files);
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
) -> axum::response::Response {
    let def = match state.registry.get_collection(slug) {
        Some(d) => d.clone(),
        None => return axum::response::Redirect::to("/admin/collections").into_response(),
    };

    // Check delete access
    match check_access_or_forbid(
        state,
        def.access.delete.as_deref(),
        auth_user,
        Some(id),
        None,
    ) {
        Ok(AccessResult::Denied) => {
            return forbidden(state, "You don't have permission to delete this item")
                .into_response()
        }
        Err(resp) => return resp,
        _ => {}
    }

    // Before hooks + delete + upload cleanup in a single transaction
    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let def_clone = def.clone();
    let slug_owned = slug.to_string();
    let id_owned = id.to_string();
    let user_doc = get_user_doc(auth_user).cloned();
    let config_dir = state.config_dir.clone();
    let result = tokio::task::spawn_blocking(move || {
        service::delete_document(
            &pool,
            &runner,
            &slug_owned,
            &id_owned,
            &def_clone,
            user_doc.as_ref(),
            Some(&config_dir),
        )
    })
    .await;

    match result {
        Ok(Ok(_req_context)) => {
            state.hook_runner.publish_event(
                &state.event_bus,
                &def.hooks,
                def.live.as_ref(),
                EventTarget::Collection,
                EventOperation::Delete,
                slug.to_string(),
                id.to_string(),
                std::collections::HashMap::new(),
                get_event_user(auth_user),
            );
        }
        Ok(Err(e)) => {
            tracing::error!("Delete error: {}", e);
        }
        Err(e) => {
            tracing::error!("Delete task error: {}", e);
        }
    }

    htmx_redirect(&format!("/admin/collections/{}", slug))
}

/// Map a FilterOp to its URL parameter name.
pub(super) fn op_to_param_name(op: &FilterOp) -> &'static str {
    match op {
        FilterOp::Equals(_) => "equals",
        FilterOp::NotEquals(_) => "not_equals",
        FilterOp::Contains(_) => "contains",
        FilterOp::Like(_) => "like",
        FilterOp::GreaterThan(_) => "gt",
        FilterOp::LessThan(_) => "lt",
        FilterOp::GreaterThanOrEqual(_) => "gte",
        FilterOp::LessThanOrEqual(_) => "lte",
        FilterOp::Exists => "exists",
        FilterOp::NotExists => "not_exists",
        _ => "equals",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;
    use crate::core::document::DocumentBuilder;
    use crate::core::field::{
        FieldAdmin, FieldDefinition, FieldType, LocalizedString, SelectOption,
    };

    fn test_collection() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Select)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Draft".into()), "draft"),
                    SelectOption::new(LocalizedString::Plain("Published".into()), "published"),
                ])
                .build(),
            FieldDefinition::builder("body", FieldType::Richtext).build(),
            FieldDefinition::builder("views", FieldType::Number).build(),
            FieldDefinition::builder("active", FieldType::Checkbox).build(),
            FieldDefinition::builder("date", FieldType::Date).build(),
        ];
        def.admin = AdminConfig {
            use_as_title: Some("title".to_string()),
            ..Default::default()
        };
        def
    }

    #[test]
    fn field_label_uses_admin_label() {
        let f = FieldDefinition::builder("my_field", FieldType::Text)
            .admin(
                FieldAdmin::builder()
                    .label(LocalizedString::Plain("Custom Label".into()))
                    .build(),
            )
            .build();
        assert_eq!(field_label(&f), "Custom Label");
    }

    #[test]
    fn field_label_falls_back_to_name() {
        let f = FieldDefinition::builder("my_field", FieldType::Text).build();
        assert_eq!(field_label(&f), "My Field");
    }

    #[test]
    fn resolve_columns_defaults() {
        let def = test_collection();
        let cols = resolve_columns(&def, None, None, "/admin/collections/posts", "", None);
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0]["key"], "created_at");
    }

    #[test]
    fn resolve_columns_user_cols() {
        let def = test_collection();
        let user_cols = vec!["status".to_string(), "views".to_string()];
        let cols = resolve_columns(
            &def,
            Some(&user_cols),
            None,
            "/admin/collections/posts",
            "",
            None,
        );
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0]["key"], "status");
        assert_eq!(cols[1]["key"], "views");
    }

    #[test]
    fn resolve_columns_filters_invalid() {
        let def = test_collection();
        let user_cols = vec!["title".to_string(), "body".to_string(), "views".to_string()];
        let cols = resolve_columns(
            &def,
            Some(&user_cols),
            None,
            "/admin/collections/posts",
            "",
            None,
        );
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0]["key"], "views");
    }

    #[test]
    fn resolve_columns_sort_state() {
        let def = test_collection();
        let user_cols = vec!["views".to_string()];
        let cols = resolve_columns(
            &def,
            Some(&user_cols),
            Some("views"),
            "/admin/collections/posts",
            "",
            None,
        );
        assert_eq!(cols[0]["is_sorted_asc"], true);
        assert_eq!(cols[0]["is_sorted_desc"], false);
    }

    #[test]
    fn resolve_columns_sort_desc_state() {
        let def = test_collection();
        let user_cols = vec!["views".to_string()];
        let cols = resolve_columns(
            &def,
            Some(&user_cols),
            Some("-views"),
            "/admin/collections/posts",
            "",
            None,
        );
        assert_eq!(cols[0]["is_sorted_asc"], false);
        assert_eq!(cols[0]["is_sorted_desc"], true);
    }

    #[test]
    fn compute_cells_status_badge() {
        let def = test_collection();
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields
            .insert("_status".into(), serde_json::json!("draft"));

        let columns = vec![serde_json::json!({"key": "_status"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["is_badge"], true);
        assert_eq!(cells[0]["value"], "draft");
    }

    #[test]
    fn compute_cells_select_shows_label() {
        let def = test_collection();
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields
            .insert("status".into(), serde_json::json!("published"));

        let columns = vec![serde_json::json!({"key": "status"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["value"], "Published");
    }

    #[test]
    fn compute_cells_checkbox() {
        let def = test_collection();
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields.insert("active".into(), serde_json::json!(1));

        let columns = vec![serde_json::json!({"key": "active"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["is_bool"], true);
        assert_eq!(cells[0]["value"], true);
    }

    #[test]
    fn compute_cells_date() {
        let def = test_collection();
        let doc = DocumentBuilder::new("1").created_at("2024-01-15").build();

        let columns = vec![serde_json::json!({"key": "created_at"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["is_date"], true);
    }

    #[test]
    fn build_column_options_includes_eligible() {
        let def = test_collection();
        let opts = build_column_options(&def, &["status".to_string()]);
        let keys: Vec<&str> = opts.iter().filter_map(|o| o["key"].as_str()).collect();
        assert!(keys.contains(&"created_at"));
        assert!(keys.contains(&"updated_at"));
        assert!(keys.contains(&"status")); // select - eligible
        assert!(keys.contains(&"views")); // number - eligible
        assert!(!keys.contains(&"body")); // richtext - ineligible
        assert!(!keys.contains(&"title")); // title field - excluded
    }

    #[test]
    fn build_column_options_marks_selected() {
        let def = test_collection();
        let opts = build_column_options(&def, &["status".to_string()]);
        let status_opt = opts.iter().find(|o| o["key"] == "status").unwrap();
        assert_eq!(status_opt["selected"], true);
        let views_opt = opts.iter().find(|o| o["key"] == "views").unwrap();
        assert_eq!(views_opt["selected"], false);
    }

    #[test]
    fn build_filter_fields_includes_eligible() {
        let def = test_collection();
        let fields = build_filter_fields(&def);
        let keys: Vec<&str> = fields.iter().filter_map(|f| f["key"].as_str()).collect();
        assert!(keys.contains(&"created_at"));
        assert!(keys.contains(&"status"));
        assert!(keys.contains(&"views"));
        assert!(!keys.contains(&"body")); // richtext ineligible
    }

    #[test]
    fn build_filter_fields_select_has_options() {
        let def = test_collection();
        let fields = build_filter_fields(&def);
        let status_field = fields.iter().find(|f| f["key"] == "status").unwrap();
        let opts = status_field["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["value"], "draft");
    }

    #[test]
    fn op_to_param_name_all_ops() {
        assert_eq!(op_to_param_name(&FilterOp::Equals("x".into())), "equals");
        assert_eq!(
            op_to_param_name(&FilterOp::NotEquals("x".into())),
            "not_equals"
        );
        assert_eq!(
            op_to_param_name(&FilterOp::Contains("x".into())),
            "contains"
        );
        assert_eq!(op_to_param_name(&FilterOp::GreaterThan("x".into())), "gt");
        assert_eq!(op_to_param_name(&FilterOp::LessThan("x".into())), "lt");
        assert_eq!(
            op_to_param_name(&FilterOp::GreaterThanOrEqual("x".into())),
            "gte"
        );
        assert_eq!(
            op_to_param_name(&FilterOp::LessThanOrEqual("x".into())),
            "lte"
        );
        assert_eq!(op_to_param_name(&FilterOp::Exists), "exists");
        assert_eq!(op_to_param_name(&FilterOp::NotExists), "not_exists");
    }

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
        assert!(arr
            .iter()
            .any(|f| f["name"] == "mime_type" && f["value"] == "image/jpeg"));
        assert!(arr
            .iter()
            .any(|f| f["name"] == "url" && f["value"] == "/uploads/media/test.jpg"));
        assert!(arr
            .iter()
            .any(|f| f["name"] == "width" && f["value"] == "1920"));
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
}
