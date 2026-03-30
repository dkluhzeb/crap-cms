use anyhow::Context as _;
use anyhow::Error;
use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::task;

use crate::{
    admin::{
        AdminState,
        context::{Breadcrumb, ContextBuilder, PageType},
        handlers::shared::{
            EnrichOptions, apply_display_conditions, build_field_contexts,
            build_locale_template_data, check_access_or_forbid, compute_denied_read_fields,
            enrich_field_contexts, extract_editor_locale, fetch_version_sidebar_data,
            flatten_document_values, forbidden, is_non_default_locale, not_found, render_or_error,
            server_error, split_sidebar_fields, strip_denied_fields,
        },
    },
    core::{
        CollectionDefinition, Document,
        auth::{AuthUser, Claims},
        upload,
    },
    db::{ops, query, query::AccessResult},
    hooks::lifecycle::AfterReadCtx,
};

/// GET /admin/collections/{slug}/{id} — show edit form
pub async fn edit_form(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => {
            return not_found(&state, &format!("Collection '{}' not found", slug));
        }
    };

    // Check read access
    let access_result = match check_access_or_forbid(
        &state,
        def.access.read.as_deref(),
        &auth_user,
        Some(&id),
        None,
    ) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    if matches!(access_result, AccessResult::Denied) {
        return forbidden(&state, "You don't have permission to view this item");
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
    let user_doc = auth_user.as_ref().map(|Extension(au)| au.user_doc.clone());
    let user_ui_locale = auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone());

    let read_result = task::spawn_blocking(move || {
        runner.fire_before_read(&hooks, &slug_owned, "find_by_id", HashMap::new())?;

        let conn = pool.get().context("DB connection")?;
        let mut doc = ops::find_by_id_full(
            &conn,
            &slug_owned,
            &def_owned,
            &id_owned,
            locale_ctx.as_ref(),
            access_constraints,
            has_drafts,
        )?;

        // Assemble sizes for upload collections
        if let Some(ref mut d) = doc
            && let Some(ref upload_config) = def_owned.upload
            && upload_config.enabled
        {
            upload::assemble_sizes_object(d, upload_config);
        }

        let ar_ctx = AfterReadCtx {
            hooks: &hooks,
            fields: &fields,
            collection: &slug_owned,
            operation: "find_by_id",
            user: user_doc.as_ref(),
            ui_locale: user_ui_locale.as_deref(),
        };
        let doc = doc.map(|d| runner.apply_after_read(&ar_ctx, d));

        Ok::<_, Error>(doc)
    })
    .await;

    let mut document = match read_result {
        Ok(Ok(Some(doc))) => doc,
        Ok(Ok(None)) => {
            return not_found(&state, &format!("Document '{}' not found", id));
        }
        Ok(Err(e)) => {
            tracing::error!("Document edit query error: {}", e);
            return server_error(&state, "An internal error occurred.");
        }
        Err(e) => {
            tracing::error!("Document edit task error: {}", e);
            return server_error(&state, "An internal error occurred.");
        }
    };

    // Strip field-level read-denied fields (fail closed on pool exhaustion)
    let denied = match compute_denied_read_fields(&state, &auth_user, &def.fields) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };
    strip_denied_fields(&mut document.fields, &denied);

    let values = flatten_document_values(&document.fields, &def.fields);

    let non_default_locale = is_non_default_locale(&state, editor_locale.as_deref());
    let mut fields = build_field_contexts(
        &def.fields,
        &values,
        &HashMap::new(),
        true,
        non_default_locale,
    );

    // Enrich relationship and array fields with extra data
    enrich_field_contexts(
        &mut fields,
        &def.fields,
        &document.fields,
        &state,
        &EnrichOptions::builder(&HashMap::new())
            .filter_hidden(true)
            .non_default_locale(non_default_locale)
            .doc_id(&id)
            .build(),
    );

    // Evaluate display conditions with document data
    let form_data_json = json!(document.fields);
    apply_display_conditions(
        &mut fields,
        &def.fields,
        &form_data_json,
        &state.hook_runner,
        true,
    );

    if def.is_auth_collection() {
        fields.push(json!({
            "name": "password",
            "field_type": "password",
            "label": "password",
            "required": false,
            "value": "",
            "description": "leave_blank_keep_password",
        }));

        // Add locked checkbox — read current lock state from DB
        let is_locked = state
            .pool
            .get()
            .ok()
            .and_then(|conn| query::auth::is_locked(&conn, &slug, &id).ok())
            .unwrap_or(false);
        fields.push(json!({
            "name": "_locked",
            "field_type": "checkbox",
            "label": "account_locked",
            "checked": is_locked,
            "description": "prevent_login",
        }));
    }

    // Split fields into main and sidebar
    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    // Determine document title for breadcrumb
    let doc_title = def
        .title_field()
        .and_then(|f| document.get_str(f))
        .map(|s| s.to_string())
        .unwrap_or_else(|| document.id.to_string());

    // Fetch document status and version history for versioned collections
    let has_versions = def.has_versions();
    let doc_status = if has_drafts {
        document
            .fields
            .get("_status")
            .and_then(|v| v.as_str())
            .unwrap_or("published")
            .to_string()
    } else {
        String::new()
    };
    let (versions, total_versions): (Vec<Value>, i64) = if has_versions {
        fetch_version_sidebar_data(&state.pool, &slug, &document.id)
    } else {
        (vec![], 0)
    };

    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let mut data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .filter_nav_by_access(&state, &auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionEdit, "edit_name")
        .page_title_name(def.singular_name())
        .collection_def(&def)
        .document_with_status(&document, &doc_status)
        .fields(main_fields)
        .set("sidebar_fields", json!(sidebar_fields))
        .set("editing", json!(true))
        .set("has_drafts", json!(has_drafts))
        .set("has_versions", json!(has_versions))
        .set("versions", json!(versions))
        .set("has_more_versions", json!(total_versions > 3))
        .set(
            "restore_url_prefix",
            json!(format!("/admin/collections/{}/{}", slug, id)),
        )
        .set(
            "versions_url",
            json!(format!("/admin/collections/{}/{}/versions", slug, id)),
        )
        .breadcrumbs(vec![
            Breadcrumb::link("collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::current(doc_title.clone()),
        ])
        .merge(locale_data)
        .build();

    data["document_title"] = json!(doc_title);

    // Add reference count for delete protection UI
    let ref_count = state
        .pool
        .get()
        .ok()
        .and_then(|conn| query::ref_count::get_ref_count(&conn, &slug, &id).ok())
        .flatten()
        .unwrap_or(0);
    data["ref_count"] = json!(ref_count);

    // Add upload context for upload collections
    if def.is_upload_collection() {
        data["upload"] = build_upload_context(&def, &document);
    }

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/edit", &data)
}

/// Build upload preview/info context for upload collection edit forms.
fn build_upload_context(def: &CollectionDefinition, document: &Document) -> Value {
    let mut ctx = json!({});

    if let Some(ref u) = def.upload
        && !u.mime_types.is_empty()
    {
        ctx["accept"] = json!(u.mime_types.join(","));
    }

    let url = document.fields.get("url").and_then(|v| v.as_str());
    let mime_type = document.fields.get("mime_type").and_then(|v| v.as_str());
    let filename = document.fields.get("filename").and_then(|v| v.as_str());
    let filesize = document
        .fields
        .get("filesize")
        .and_then(|v| v.as_f64())
        .map(|v| v as u64);
    let width = document
        .fields
        .get("width")
        .and_then(|v| v.as_f64())
        .map(|v| v as u32);
    let height = document
        .fields
        .get("height")
        .and_then(|v| v.as_f64())
        .map(|v| v as u32);

    // Focal point values
    if let Some(fx) = document.fields.get("focal_x").and_then(|v| v.as_f64()) {
        ctx["focal_x"] = json!(fx);
    }
    if let Some(fy) = document.fields.get("focal_y").and_then(|v| v.as_f64()) {
        ctx["focal_y"] = json!(fy);
    }

    // Show preview for images
    if let (Some(url), Some(mime)) = (url, mime_type)
        && mime.starts_with("image/")
    {
        let preview_url = def
            .upload
            .as_ref()
            .and_then(|u| u.admin_thumbnail.as_ref())
            .and_then(|thumb_name| {
                document
                    .fields
                    .get("sizes")
                    .and_then(|v| v.get(thumb_name))
                    .and_then(|v| v.get("url"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| url.to_string());
        ctx["preview"] = json!(preview_url);
    }

    if let Some(fname) = filename {
        let mut info = json!({ "filename": fname });
        if let Some(size) = filesize {
            info["filesize_display"] = json!(upload::format_filesize(size));
        }
        if let (Some(w), Some(h)) = (width, height) {
            info["dimensions"] = json!(format!("{}x{}", w, h));
        }
        ctx["info"] = info;
    }

    ctx
}
