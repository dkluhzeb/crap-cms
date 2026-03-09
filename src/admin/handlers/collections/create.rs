//! Collection create handlers.

use axum::{
    extract::{Form, FromRequest, Path, State},
    response::IntoResponse,
    Extension,
};
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::admin::context::{ContextBuilder, PageType, Breadcrumb};
use crate::core::auth::{AuthUser, Claims};
use crate::core::upload;
use crate::core::validate::ValidationError;
use crate::db::query::{AccessResult, LocaleContext};

use super::{
    get_user_doc, get_event_user, check_access_or_forbid, extract_editor_locale,
    build_locale_template_data, is_non_default_locale,
    build_field_contexts, enrich_field_contexts,
    apply_display_conditions, split_sidebar_fields,
    redirect_response, htmx_redirect, html_with_toast,
    render_or_error, not_found, server_error, forbidden,
};

use crate::core::upload::inject_upload_metadata;
use super::forms::{extract_join_data_from_form, parse_multipart_form, transform_select_has_many};

/// Collect hidden upload field values from form data for re-rendering after validation errors.
/// Returns a JSON array of `{ "name": "...", "value": "..." }` objects for hidden `<input>` elements.
pub(super) fn collect_upload_hidden_fields(
    fields: &[crate::core::field::FieldDefinition],
    form_data: &HashMap<String, String>,
) -> serde_json::Value {
    let hidden_fields: Vec<serde_json::Value> = fields.iter()
        .filter(|f| f.admin.hidden)
        .filter_map(|f| {
            form_data.get(&f.name).map(|v| {
                serde_json::json!({"name": &f.name, "value": v})
            })
        })
        .collect();
    serde_json::json!(hidden_fields)
}

/// GET /admin/collections/{slug}/create — show create form
pub async fn create_form(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    headers: axum::http::HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return not_found(&state, &format!("Collection '{}' not found", slug)).into_response(),
    };

    // Check create access
    match check_access_or_forbid(
        &state, def.access.create.as_deref(), &auth_user, None, None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to create items in this collection").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let non_default_locale = is_non_default_locale(&state, editor_locale.as_deref());
    let mut fields = build_field_contexts(&def.fields, &HashMap::new(), &HashMap::new(), true, non_default_locale);

    // Enrich relationship and array fields
    enrich_field_contexts(&mut fields, &def.fields, &HashMap::new(), &state, true, non_default_locale, &HashMap::new(), None);

    // Evaluate display conditions (empty form data for create)
    let empty_data = serde_json::json!({});
    apply_display_conditions(&mut fields, &def.fields, &empty_data, &state.hook_runner, true);

    if def.is_auth_collection() {
        fields.push(serde_json::json!({
            "name": "password",
            "field_type": "password",
            "label": "Password",
            "required": true,
            "value": "",
            "description": "Set the user's password",
        }));
    }

    // Split fields into main and sidebar
    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    let (_locale_ctx, locale_data) = build_locale_template_data(&state, editor_locale.as_deref());

    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let mut data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionCreate, format!("Create {}", def.singular_name()))
        .set("page_title", serde_json::json!(format!("Create {}", def.singular_name())))
        .collection_def(&def)
        .fields(main_fields)
        .set("sidebar_fields", serde_json::json!(sidebar_fields))
        .set("editing", serde_json::json!(false))
        .set("has_drafts", serde_json::json!(def.has_drafts()))
        .breadcrumbs(vec![
            Breadcrumb::link("Collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::current(format!("Create {}", def.singular_name())),
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
        data["upload"] = upload_ctx;
    }

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/edit", &data).into_response()
}

/// POST /admin/collections/{slug} — create a new item
pub async fn create_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin/collections"),
    };

    // Check create access
    match check_access_or_forbid(
        &state, def.access.create.as_deref(), &auth_user, None, None,
    ) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to create items in this collection").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    // Parse form data — multipart for upload collections, regular form for others
    let (mut form_data, file) = if def.is_upload_collection() {
        match parse_multipart_form(request, &state).await {
            Ok(result) => result,
            Err(e) => {
                tracing::error!("Multipart parse error: {}", e);
                return redirect_response(&format!("/admin/collections/{}/create", slug));
            }
        }
    } else {
        let Form(data) = match Form::<HashMap<String, String>>::from_request(request, &state).await {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("Form parse error: {}", e);
                return redirect_response(&format!("/admin/collections/{}/create", slug));
            }
        };
        (data, None)
    };

    // Process upload if file present
    let mut queued_conversions = Vec::new();
    if let Some(f) = file {
        if let Some(ref upload_config) = def.upload {
            match upload::process_upload(
                &f, upload_config, &state.config_dir, &slug,
                state.config.upload.max_file_size,
            ) {
                Ok(processed) => {
                    queued_conversions = processed.queued_conversions.clone();
                    inject_upload_metadata(&mut form_data, &processed);
                }
                Err(e) => {
                    tracing::error!("Upload processing error: {}", e);
                    let mut fields = build_field_contexts(&def.fields, &form_data, &HashMap::new(), true, false);
                    enrich_field_contexts(&mut fields, &def.fields, &HashMap::new(), &state, true, false, &HashMap::new(), None);
                    let empty_data = serde_json::json!({});
                    apply_display_conditions(&mut fields, &def.fields, &empty_data, &state.hook_runner, true);
                    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);
                    let data = ContextBuilder::new(&state, None)
                        .locale_from_auth(&auth_user)
                        .page(PageType::CollectionCreate, format!("Create {}", def.singular_name()))
                        .set("page_title", serde_json::json!(format!("Create {}", def.singular_name())))
                        .collection_def(&def)
                        .fields(main_fields)
                        .set("sidebar_fields", serde_json::json!(sidebar_fields))
                        .set("editing", serde_json::json!(false))
                        .set("has_drafts", serde_json::json!(def.has_drafts()))
                        .build();
                    return html_with_toast(&state, "collections/edit", &data, &e.to_string());
                }
            }
        }
    }

    // Strip field-level create-denied fields (fail closed on pool exhaustion)
    if def.fields.iter().any(|f| f.access.create.is_some()) {
        let user_doc = get_user_doc(&auth_user);
        let mut conn = match state.pool.get() {
            Ok(c) => c,
            Err(e) => { tracing::error!("Field access check pool error: {}", e); return server_error(&state, "Database error").into_response(); }
        };
        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(e) => { tracing::error!("Field access check tx error: {}", e); return server_error(&state, "Database error").into_response(); }
        };
        let denied = state.hook_runner.check_field_write_access(&def.fields, user_doc, "create", &tx);
        // Read-only access check — commit result is irrelevant, rollback on drop is safe
        let _ = tx.commit();
        for name in &denied {
            form_data.remove(name);
        }
    }

    // Extract password before it enters hooks/regular data flow
    let password = if def.is_auth_collection() {
        form_data.remove("password")
    } else {
        None
    };

    // Validate password against policy (create requires a password for auth collections)
    if let Some(ref pw) = password {
        if !pw.is_empty() {
            if let Err(e) = state.config.auth.password_policy.validate(pw) {
                return html_with_toast(&state, "collections/edit_form", &serde_json::json!({}), &e.to_string()).into_response();
            }
        }
    }

    // Convert comma-separated multi-select values to JSON arrays
    transform_select_has_many(&mut form_data, &def.fields);

    // Extract join table data (arrays + has-many relationships) from form
    let join_data = extract_join_data_from_form(&form_data, &def.fields);

    // Extract action (publish/save_draft) and locale before they enter hooks
    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let form_locale = form_data.remove("_locale");
    let locale_ctx = LocaleContext::from_locale_string(
        form_locale.as_deref(), &state.config.locale,
    );

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let form_data_clone = form_data.clone();
    let join_data_clone = join_data.clone();
    let user_doc = get_user_doc(&auth_user).cloned();
    let locale = locale_ctx.as_ref().and_then(|ctx| match &ctx.mode {
        crate::db::query::LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });
    let ui_locale = auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone());
    let result = tokio::task::spawn_blocking(move || {
        crate::service::create_document(
            &pool, &runner, &slug_owned, &def_owned,
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
    }).await;

    match result {
        Ok(Ok((doc, _req_context))) => {
            // Enqueue deferred image conversions if any
            if !queued_conversions.is_empty() {
                if let Ok(conn) = state.pool.get() {
                    if let Err(e) = upload::enqueue_conversions(&conn, &slug, &doc.id, &queued_conversions) {
                        tracing::warn!("Failed to enqueue image conversions: {}", e);
                    }
                }
            }

            state.hook_runner.publish_event(
                &state.event_bus, &def.hooks, def.live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Create,
                slug.clone(), doc.id.clone(), doc.fields.clone(),
                get_event_user(&auth_user),
            );

            // Auto-send verification email for auth collections with verify_email enabled
            if def.is_auth_collection() && def.auth.as_ref().is_some_and(|a| a.verify_email) {
                if let Some(user_email) = doc.fields.get("email").and_then(|v| v.as_str()) {
                    crate::service::send_verification_email(
                        state.pool.clone(),
                        state.config.email.clone(),
                        state.email_renderer.clone(),
                        state.config.server.clone(),
                        slug.clone(),
                        doc.id.clone(),
                        user_email.to_string(),
                    );
                }
            }

            htmx_redirect(&format!("/admin/collections/{}", slug))
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let error_map = ve.to_field_map();
                let mut fields = build_field_contexts(&def.fields, &form_data_clone, &error_map, true, false);
                enrich_field_contexts(&mut fields, &def.fields, &join_data_clone, &state, true, false, &error_map, None);
                let form_json = serde_json::json!(form_data_clone.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect::<serde_json::Map<String, serde_json::Value>>());
                apply_display_conditions(&mut fields, &def.fields, &form_json, &state.hook_runner, true);
                let (main_fields, sidebar_fields) = split_sidebar_fields(fields);
                let mut data = ContextBuilder::new(&state, None)
                    .locale_from_auth(&auth_user)
                    .page(PageType::CollectionCreate, format!("Create {}", def.singular_name()))
                    .set("page_title", serde_json::json!(format!("Create {}", def.singular_name())))
                    .collection_def(&def)
                    .fields(main_fields)
                    .set("sidebar_fields", serde_json::json!(sidebar_fields))
                    .set("editing", serde_json::json!(false))
                    .set("has_drafts", serde_json::json!(def.has_drafts()))
                    .build();
                // Preserve upload metadata as hidden inputs so they survive form re-submission
                if def.is_upload_collection() {
                    data["upload_hidden_fields"] = collect_upload_hidden_fields(&def.fields, &form_data_clone);
                }
                html_with_toast(&state, "collections/edit", &data, &e.to_string())
            } else {
                tracing::error!("Create error: {}", e);
                redirect_response(&format!("/admin/collections/{}/create", slug))
            }
        }
        Err(e) => {
            tracing::error!("Create task error: {}", e);
            redirect_response(&format!("/admin/collections/{}/create", slug))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldAdmin, FieldDefinition, FieldType};
    use std::collections::HashMap;

    #[test]
    fn collect_upload_hidden_fields_basic() {
        let fields = vec![
            FieldDefinition::builder("filename", FieldType::Text).build(),
            FieldDefinition::builder("mime_type", FieldType::Text).admin(FieldAdmin::builder().hidden(true).build()).build(),
            FieldDefinition::builder("url", FieldType::Text).admin(FieldAdmin::builder().hidden(true).build()).build(),
            FieldDefinition::builder("width", FieldType::Number).admin(FieldAdmin::builder().hidden(true).build()).build(),
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

        // Only hidden fields: mime_type, url, width (not filename or alt — they're not hidden)
        assert_eq!(arr.len(), 3);
        assert!(arr.iter().any(|f| f["name"] == "mime_type" && f["value"] == "image/jpeg"));
        assert!(arr.iter().any(|f| f["name"] == "url" && f["value"] == "/uploads/media/test.jpg"));
        assert!(arr.iter().any(|f| f["name"] == "width" && f["value"] == "1920"));
    }

    #[test]
    fn collect_upload_hidden_fields_missing_values() {
        let fields = vec![
            FieldDefinition::builder("url", FieldType::Text).admin(FieldAdmin::builder().hidden(true).build()).build(),
            FieldDefinition::builder("mime_type", FieldType::Text).admin(FieldAdmin::builder().hidden(true).build()).build(),
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
        let fields = vec![
            FieldDefinition::builder("alt", FieldType::Text).build(),
        ];
        let form_data = HashMap::new();
        let result = collect_upload_hidden_fields(&fields, &form_data);
        assert_eq!(result.as_array().unwrap().len(), 0);
    }
}
