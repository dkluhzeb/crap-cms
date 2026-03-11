use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Extension,
};
use anyhow::Context as _;
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::admin::context::{ContextBuilder, PageType, Breadcrumb};
use crate::core::auth::{AuthUser, Claims};
use crate::core::upload;
use crate::db::{ops, query};
use crate::db::query::AccessResult;

use crate::core::field::FieldType;
use crate::admin::handlers::shared::{
    get_user_doc, strip_denied_fields, check_access_or_forbid,
    extract_editor_locale, build_locale_template_data, is_non_default_locale,
    build_field_contexts, enrich_field_contexts,
    apply_display_conditions, split_sidebar_fields,
    fetch_version_sidebar_data,
    render_or_error, not_found, server_error, forbidden,
};

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
                if def.fields.iter().any(|f| f.name == *k && f.field_type == FieldType::Group) {
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
