//! Collection edit handlers: edit form, update action.

use axum::{
    extract::{Form, FromRequest, Path, State},
    response::IntoResponse,
    Extension,
};
use anyhow::Context as _;
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::admin::context::{ContextBuilder, PageType, Breadcrumb};
use crate::core::auth::{AuthUser, Claims};
use crate::core::upload::{self, UploadedFile};
use crate::core::validate::ValidationError;
use crate::db::{ops, query};
use crate::db::query::{AccessResult, LocaleContext};

use super::{
    get_user_doc, get_event_user, strip_denied_fields, check_access_or_forbid,
    extract_editor_locale, build_locale_template_data, is_non_default_locale,
    build_field_contexts, enrich_field_contexts,
    apply_display_conditions, split_sidebar_fields,
    translate_validation_errors,
    fetch_version_sidebar_data,
    forbidden, redirect_response, htmx_redirect, html_with_toast,
    render_or_error, not_found, server_error,
};

use crate::core::upload::inject_upload_metadata;
use super::forms::{extract_join_data_from_form, parse_multipart_form, transform_select_has_many};
use super::create::collect_upload_hidden_fields;

/// GET /admin/collections/{slug}/{id} — show edit form
pub async fn edit_form(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return not_found(&state, &format!("Collection '{}' not found", slug)).into_response(),
    };

    // Check read access
    let access_result = match check_access_or_forbid(
        &state, def.access.read.as_deref(), &auth_user, Some(&id), None,
    ) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if matches!(access_result, AccessResult::Denied) {
        return forbidden(&state, "You don't have permission to view this item").into_response();
    }

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let (locale_ctx, locale_data) = build_locale_template_data(&state, editor_locale.as_deref());

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let hooks = def.hooks.clone();
    let fields = def.fields.clone();
    let slug_owned = slug.clone();
    let id_owned = id.clone();
    let def_owned = def.clone();
    let access_constraints = if let AccessResult::Constrained(ref filters) = access_result {
        Some(filters.clone())
    } else {
        None
    };
    let has_drafts = def.has_drafts();
    let read_result = tokio::task::spawn_blocking(move || {
        runner.fire_before_read(&hooks, &slug_owned, "find_by_id", HashMap::new())?;
        let conn = pool.get().context("DB connection")?;
        let mut doc = ops::find_by_id_full(
            &conn, &slug_owned, &def_owned, &id_owned,
            locale_ctx.as_ref(), access_constraints, has_drafts,
        )?;
        // Assemble sizes for upload collections
        if let Some(ref mut d) = doc {
            if let Some(ref upload_config) = def_owned.upload {
                if upload_config.enabled {
                    upload::assemble_sizes_object(d, upload_config);
                }
            }
        }
        let doc = doc.map(|d| runner.apply_after_read(&hooks, &fields, &slug_owned, "find_by_id", d, None, None));
        Ok::<_, anyhow::Error>(doc)
    }).await;

    let mut document = match read_result {
        Ok(Ok(Some(doc))) => doc,
        Ok(Ok(None)) => return not_found(&state, &format!("Document '{}' not found", id)).into_response(),
        Ok(Err(e)) => { tracing::error!("Document edit query error: {}", e); return server_error(&state, "An internal error occurred.").into_response(); }
        Err(e) => { tracing::error!("Document edit task error: {}", e); return server_error(&state, "An internal error occurred.").into_response(); }
    };

    // Strip field-level read-denied fields (fail closed on pool exhaustion)
    if def.fields.iter().any(|f| f.access.read.is_some()) {
        let user_doc = get_user_doc(&auth_user);
        let mut conn = match state.pool.get() {
            Ok(c) => c,
            Err(e) => { tracing::error!("Field access check pool error: {}", e); return server_error(&state, "Database error").into_response(); }
        };
        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(e) => { tracing::error!("Field access check tx error: {}", e); return server_error(&state, "Database error").into_response(); }
        };
        let denied = state.hook_runner.check_field_read_access(&def.fields, user_doc, &tx);
        // Read-only access check — commit result is irrelevant, rollback on drop is safe
        let _ = tx.commit();
        strip_denied_fields(&mut document.fields, &denied);
    }

    let values: HashMap<String, String> = document.fields.iter()
        .flat_map(|(k, v)| {
            // Group fields are hydrated as nested objects — flatten back to
            // prefixed column names (e.g. location → location__venue_name)
            // so that build_field_contexts can find the sub-field values.
            if let serde_json::Value::Object(obj) = v {
                if def.fields.iter().any(|f| f.name == *k && f.field_type == crate::core::field::FieldType::Group) {
                    return obj.iter().map(|(sub_k, sub_v)| {
                        let col = format!("{}__{}", k, sub_k);
                        let s = match sub_v {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Number(n) => n.to_string(),
                            serde_json::Value::Bool(b) => b.to_string(),
                            serde_json::Value::Null => String::new(),
                            other => other.to_string(),
                        };
                        (col, s)
                    }).collect::<Vec<_>>();
                }
            }
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            vec![(k.clone(), s)]
        })
        .collect();

    let non_default_locale = is_non_default_locale(&state, editor_locale.as_deref());
    let mut fields = build_field_contexts(&def.fields, &values, &HashMap::new(), true, non_default_locale);

    // Enrich relationship and array fields with extra data
    enrich_field_contexts(&mut fields, &def.fields, &document.fields, &state, true, non_default_locale, &HashMap::new(), Some(&id));

    // Evaluate display conditions with document data
    let form_data_json = serde_json::json!(document.fields);
    apply_display_conditions(&mut fields, &def.fields, &form_data_json, &state.hook_runner, true);

    if def.is_auth_collection() {
        fields.push(serde_json::json!({
            "name": "password",
            "field_type": "password",
            "label": "Password",
            "required": false,
            "value": "",
            "description": "Leave blank to keep current password",
        }));

        // Add locked checkbox — read current lock state from DB
        let is_locked = state.pool.get().ok()
            .and_then(|conn| query::auth::is_locked(&conn, &slug, &id).ok())
            .unwrap_or(false);
        fields.push(serde_json::json!({
            "name": "_locked",
            "field_type": "checkbox",
            "label": "Account locked",
            "checked": is_locked,
            "description": "Prevent this user from logging in",
        }));
    }

    // Split fields into main and sidebar
    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    // Determine document title for breadcrumb
    let doc_title = def.title_field()
        .and_then(|f| document.get_str(f))
        .map(|s| s.to_string())
        .unwrap_or_else(|| document.id.clone());

    // Fetch document status and version history for versioned collections
    let has_versions = def.has_versions();
    let doc_status = if has_drafts {
        document.fields.get("_status")
            .and_then(|v| v.as_str())
            .unwrap_or("published")
            .to_string()
    } else {
        String::new()
    };
    let (versions, total_versions): (Vec<serde_json::Value>, i64) = if has_versions {
        fetch_version_sidebar_data(&state.pool, &slug, &document.id)
    } else {
        (vec![], 0)
    };

    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let mut data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionEdit, format!("Edit {}", def.singular_name()))
        .set("page_title", serde_json::json!(format!("Edit {}", def.singular_name())))
        .collection_def(&def)
        .document_with_status(&document, &doc_status)
        .fields(main_fields)
        .set("sidebar_fields", serde_json::json!(sidebar_fields))
        .set("editing", serde_json::json!(true))
        .set("has_drafts", serde_json::json!(has_drafts))
        .set("has_versions", serde_json::json!(has_versions))
        .set("versions", serde_json::json!(versions))
        .set("has_more_versions", serde_json::json!(total_versions > 3))
        .set("restore_url_prefix", serde_json::json!(format!("/admin/collections/{}/{}", slug, id)))
        .set("versions_url", serde_json::json!(format!("/admin/collections/{}/{}/versions", slug, id)))
        .breadcrumbs(vec![
            Breadcrumb::link("Collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::current(doc_title),
        ])
        .merge(locale_data)
        .build();

    // Add upload context for upload collections
    if def.is_upload_collection() {
        let mut upload_ctx = serde_json::json!({});
        if let Some(ref u) = def.upload {
            if !u.mime_types.is_empty() {
                upload_ctx["accept"] = serde_json::json!(u.mime_types.join(","));
            }
        }

        // Upload preview and file info from existing document
        let url = document.fields.get("url").and_then(|v| v.as_str());
        let mime_type = document.fields.get("mime_type").and_then(|v| v.as_str());
        let filename = document.fields.get("filename").and_then(|v| v.as_str());
        let filesize = document.fields.get("filesize").and_then(|v| v.as_f64()).map(|v| v as u64);
        let width = document.fields.get("width").and_then(|v| v.as_f64()).map(|v| v as u32);
        let height = document.fields.get("height").and_then(|v| v.as_f64()).map(|v| v as u32);

        // Pass focal point values
        let focal_x = document.fields.get("focal_x").and_then(|v| v.as_f64());
        let focal_y = document.fields.get("focal_y").and_then(|v| v.as_f64());
        if let Some(fx) = focal_x {
            upload_ctx["focal_x"] = serde_json::json!(fx);
        }
        if let Some(fy) = focal_y {
            upload_ctx["focal_y"] = serde_json::json!(fy);
        }

        // Show preview for images
        if let (Some(url), Some(mime)) = (url, mime_type) {
            if mime.starts_with("image/") {
                // Use admin_thumbnail size if available
                let preview_url = def.upload.as_ref()
                    .and_then(|u| u.admin_thumbnail.as_ref())
                    .and_then(|thumb_name| {
                        document.fields.get("sizes")
                            .and_then(|v| v.get(thumb_name))
                            .and_then(|v| v.get("url"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_else(|| url.to_string());
                upload_ctx["preview"] = serde_json::json!(preview_url);
            }
        }

        if let Some(fname) = filename {
            let mut info = serde_json::json!({
                "filename": fname,
            });
            if let Some(size) = filesize {
                info["filesize_display"] = serde_json::json!(upload::format_filesize(size));
            }
            if let (Some(w), Some(h)) = (width, height) {
                info["dimensions"] = serde_json::json!(format!("{}x{}", w, h));
            }
            upload_ctx["info"] = info;
        }
        data["upload"] = upload_ctx;
    }

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/edit", &data).into_response()
}

/// POST handler for update/delete (HTML forms use _method override).
pub async fn update_action_post(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin/collections"),
    };

    // Parse form data — multipart for upload collections, regular form for others
    let (mut form_data, file) = if def.is_upload_collection() {
        match parse_multipart_form(request, &state).await {
            Ok(result) => result,
            Err(e) => {
                tracing::error!("Multipart parse error: {}", e);
                return redirect_response(&format!("/admin/collections/{}/{}", slug, id));
            }
        }
    } else {
        let Form(data) = match Form::<HashMap<String, String>>::from_request(request, &state).await {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("Form parse error: {}", e);
                return redirect_response(&format!("/admin/collections/{}/{}", slug, id));
            }
        };
        (data, None)
    };

    let method = form_data.remove("_method").unwrap_or_default();

    if method.eq_ignore_ascii_case("DELETE") {
        return super::delete::delete_action_impl(&state, &slug, &id, &auth_user).await.into_response();
    }

    do_update(&state, &slug, &id, form_data, file, &auth_user).await
}

pub(super) async fn do_update(state: &AdminState, slug: &str, id: &str, mut form_data: HashMap<String, String>, file: Option<UploadedFile>, auth_user: &Option<Extension<AuthUser>>) -> axum::response::Response {
    let def = match state.registry.get_collection(slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin/collections"),
    };

    // Extract action (publish/save_draft/unpublish) and locale
    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let form_locale = form_data.remove("_locale");
    let locale_ctx = LocaleContext::from_locale_string(
        form_locale.as_deref(), &state.config.locale,
    );

    // Check update access
    match check_access_or_forbid(
        state, def.access.update.as_deref(), auth_user, Some(id), None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(state, "You don't have permission to update this item").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    // For upload collections, if a new file was uploaded, process it and delete old files
    let mut old_doc_fields: Option<HashMap<String, serde_json::Value>> = None;
    let mut queued_conversions = Vec::new();
    if let Some(f) = file {
        if let Some(ref upload_config) = def.upload {
            // Load old document to get old file paths for cleanup
            if let Ok(conn) = state.pool.get() {
                if let Ok(Some(old_doc)) = query::find_by_id(&conn, slug, &def, id, locale_ctx.as_ref()) {
                    old_doc_fields = Some(old_doc.fields.clone());
                }
            }

            match upload::process_upload(
                &f, upload_config, &state.config_dir, slug,
                state.config.upload.max_file_size,
            ) {
                Ok(processed) => {
                    queued_conversions = processed.queued_conversions.clone();
                    inject_upload_metadata(&mut form_data, &processed);
                }
                Err(e) => {
                    tracing::error!("Upload processing error: {}", e);
                    let mut fields = build_field_contexts(&def.fields, &form_data, &HashMap::new(), true, false);
                    enrich_field_contexts(&mut fields, &def.fields, &HashMap::new(), state, true, false, &HashMap::new(), Some(id));
                    let form_json = serde_json::json!(form_data.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect::<serde_json::Map<String, serde_json::Value>>());
                    apply_display_conditions(&mut fields, &def.fields, &form_json, &state.hook_runner, true);
                    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);
                    let data = ContextBuilder::new(state, None)
                        .locale_from_auth(auth_user)
                        .page(PageType::CollectionEdit, format!("Edit {}", def.singular_name()))
                        .set("page_title", serde_json::json!(format!("Edit {}", def.singular_name())))
                        .collection_def(&def)
                        .document_stub(id)
                        .fields(main_fields)
                        .set("sidebar_fields", serde_json::json!(sidebar_fields))
                        .set("editing", serde_json::json!(true))
                        .set("has_drafts", serde_json::json!(def.has_drafts()))
                        .build();
                    return html_with_toast(state, "collections/edit", &data, &e.to_string());
                }
            }
        }
    }

    // Strip field-level update-denied fields (fail closed on pool exhaustion)
    if def.fields.iter().any(|f| f.access.update.is_some()) {
        let user_doc = get_user_doc(auth_user);
        let mut conn = match state.pool.get() {
            Ok(c) => c,
            Err(e) => { tracing::error!("Field access check pool error: {}", e); return server_error(state, "Database error").into_response(); }
        };
        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(e) => { tracing::error!("Field access check tx error: {}", e); return server_error(state, "Database error").into_response(); }
        };
        let denied = state.hook_runner.check_field_write_access(&def.fields, user_doc, "update", &tx);
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
                return html_with_toast(state, "collections/edit_form", &serde_json::json!({}), &e.to_string()).into_response();
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
        crate::db::query::LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });
    let ui_locale = auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone());
    let action_owned = action.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Handle unpublish: set _status to 'draft' and create a version
        let result = if action_owned == "unpublish" && def_owned.has_versions() {
            let doc = crate::service::unpublish_document(
                &pool, &runner, &slug_owned, &id_owned, &def_owned, user_doc.as_ref(),
            )?;
            Ok((doc, HashMap::new()))
        } else {
            crate::service::update_document(
                &pool, &runner, &slug_owned, &id_owned, &def_owned,
                crate::service::WriteInput {
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
                let should_lock = locked_field.as_deref() == Some("on")
                    || locked_field.as_deref() == Some("1");
                let conn = pool.get().context("DB connection for lock update")?;
                if should_lock {
                    query::auth::lock_user(&conn, &slug_owned, &id_owned)?;
                } else {
                    query::auth::unlock_user(&conn, &slug_owned, &id_owned)?;
                }
            }
        }

        result
    }).await;

    match result {
        Ok(Ok((doc, _req_context))) => {
            // If a new file was uploaded and old files exist, clean up old files
            if let Some(old_fields) = old_doc_fields {
                upload::delete_upload_files(&state.config_dir, &old_fields);
            }

            // Enqueue deferred image conversions if any
            if !queued_conversions.is_empty() {
                if let Ok(conn) = state.pool.get() {
                    if let Err(e) = upload::enqueue_conversions(&conn, slug, id, &queued_conversions) {
                        tracing::warn!("Failed to enqueue image conversions: {}", e);
                    }
                }
            }

            state.hook_runner.publish_event(
                &state.event_bus, &def.hooks, def.live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Update,
                slug.to_string(), id.to_string(), doc.fields.clone(),
                get_event_user(auth_user),
            );
            htmx_redirect(&format!("/admin/collections/{}/{}", slug, id))
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let locale = auth_user.as_ref()
                    .map(|Extension(au)| au.ui_locale.as_str())
                    .unwrap_or("en");
                let error_map = translate_validation_errors(ve, &state.translations, locale);
                let toast_msg = state.translations.get(locale, "validation.error_summary");
                let mut fields = build_field_contexts(&def.fields, &form_data_clone, &error_map, true, false);
                enrich_field_contexts(&mut fields, &def.fields, &join_data_clone, state, true, false, &error_map, Some(id));
                let form_json = serde_json::json!(form_data_clone.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect::<serde_json::Map<String, serde_json::Value>>());
                apply_display_conditions(&mut fields, &def.fields, &form_json, &state.hook_runner, true);
                let (main_fields, sidebar_fields) = split_sidebar_fields(fields);
                let mut data = ContextBuilder::new(state, None)
                    .locale_from_auth(auth_user)
                    .page(PageType::CollectionEdit, format!("Edit {}", def.singular_name()))
                    .set("page_title", serde_json::json!(format!("Edit {}", def.singular_name())))
                    .collection_def(&def)
                    .document_stub(id)
                    .fields(main_fields)
                    .set("sidebar_fields", serde_json::json!(sidebar_fields))
                    .set("editing", serde_json::json!(true))
                    .set("has_drafts", serde_json::json!(def.has_drafts()))
                    .build();
                // Preserve upload metadata as hidden inputs so they survive form re-submission
                if def.is_upload_collection() {
                    data["upload_hidden_fields"] = collect_upload_hidden_fields(&def.fields, &form_data_clone);
                }
                html_with_toast(state, "collections/edit", &data, toast_msg)
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
