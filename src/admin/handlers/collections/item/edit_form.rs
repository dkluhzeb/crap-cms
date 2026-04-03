use std::collections::HashMap;

use anyhow::{Context as _, Error};
use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::{Value, json};
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        context::{Breadcrumb, ContextBuilder, PageType},
        handlers::shared::{
            EnrichOptions, apply_display_conditions, build_field_contexts,
            build_locale_template_data, check_access_or_forbid, compute_denied_read_fields,
            enrich_field_contexts, extract_editor_locale, fetch_version_sidebar_data,
            flatten_document_values, forbidden, is_non_default_locale, lookup_ref_count, not_found,
            render_or_error, server_error, split_sidebar_fields, strip_denied_fields,
        },
    },
    core::{
        CollectionDefinition, Document, FieldDefinition,
        auth::{AuthUser, Claims},
        collection::Hooks,
        upload,
    },
    db::{
        DbPool, ops, query,
        query::{AccessResult, FilterClause, LocaleContext},
    },
    hooks::{HookRunner, lifecycle::AfterReadCtx},
};

/// Parameters for the blocking document-read task.
struct ReadParams {
    pool: DbPool,
    runner: HookRunner,
    hooks: Hooks,
    fields: Vec<FieldDefinition>,
    slug: String,
    id: String,
    def: CollectionDefinition,
    locale_ctx: Option<LocaleContext>,
    access_constraints: Option<Vec<FilterClause>>,
    has_drafts: bool,
    user_doc: Option<Document>,
    user_ui_locale: Option<String>,
}

/// Fetch the document, run lifecycle hooks, and assemble upload sizes.
fn read_document(params: ReadParams) -> Result<Option<Document>, Error> {
    params
        .runner
        .fire_before_read(&params.hooks, &params.slug, "find_by_id", HashMap::new())?;

    let conn = params.pool.get().context("DB connection")?;

    let mut doc = ops::find_by_id_full(
        &conn,
        &params.slug,
        &params.def,
        &params.id,
        params.locale_ctx.as_ref(),
        params.access_constraints,
        params.has_drafts,
    )?;

    if let Some(ref mut d) = doc
        && let Some(ref upload_config) = params.def.upload
        && upload_config.enabled
    {
        upload::assemble_sizes_object(d, upload_config);
    }

    let ar_ctx = AfterReadCtx {
        hooks: &params.hooks,
        fields: &params.fields,
        collection: &params.slug,
        operation: "find_by_id",
        user: params.user_doc.as_ref(),
        ui_locale: params.user_ui_locale.as_deref(),
    };

    Ok(doc.map(|d| params.runner.apply_after_read(&ar_ctx, d)))
}

/// Append auth-specific fields (password, locked checkbox) to the field list.
fn append_auth_fields(fields: &mut Vec<Value>, pool: &DbPool, slug: &str, id: &str) {
    fields.push(json!({
        "name": "password",
        "field_type": "password",
        "label": "password",
        "required": false,
        "value": "",
        "description": "leave_blank_keep_password",
    }));

    let is_locked = pool
        .get()
        .ok()
        .and_then(|conn| query::auth::is_locked(&conn, slug, id).ok())
        .unwrap_or(false);

    fields.push(json!({
        "name": "_locked",
        "field_type": "checkbox",
        "label": "account_locked",
        "checked": is_locked,
        "description": "prevent_login",
    }));
}

/// Build, enrich, and split the field contexts for the edit form.
fn prepare_edit_fields(
    state: &AdminState,
    def: &CollectionDefinition,
    document: &Document,
    id: &str,
    editor_locale: Option<&str>,
) -> (Vec<Value>, Vec<Value>) {
    let values = flatten_document_values(&document.fields, &def.fields);
    let non_default_locale = is_non_default_locale(state, editor_locale);

    let mut fields = build_field_contexts(
        &def.fields,
        &values,
        &HashMap::new(),
        true,
        non_default_locale,
    );

    enrich_field_contexts(
        &mut fields,
        &def.fields,
        &document.fields,
        state,
        &EnrichOptions::builder(&HashMap::new())
            .filter_hidden(true)
            .non_default_locale(non_default_locale)
            .doc_id(id)
            .build(),
    );

    let form_data_json = json!(document.fields);
    apply_display_conditions(
        &mut fields,
        &def.fields,
        &form_data_json,
        &state.hook_runner,
        true,
    );

    if def.is_auth_collection() {
        append_auth_fields(&mut fields, &state.pool, &def.slug, id);
    }

    split_sidebar_fields(fields)
}

/// Extract file metadata fields from a document for upload context.
struct UploadMeta<'a> {
    url: Option<&'a str>,
    mime_type: Option<&'a str>,
    filename: Option<&'a str>,
    filesize: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    focal_x: Option<f64>,
    focal_y: Option<f64>,
}

impl<'a> UploadMeta<'a> {
    fn from_document(document: &'a Document) -> Self {
        Self {
            url: document.fields.get("url").and_then(|v| v.as_str()),
            mime_type: document.fields.get("mime_type").and_then(|v| v.as_str()),
            filename: document.fields.get("filename").and_then(|v| v.as_str()),
            filesize: document
                .fields
                .get("filesize")
                .and_then(|v| v.as_f64())
                .map(|v| v as u64),
            width: document
                .fields
                .get("width")
                .and_then(|v| v.as_f64())
                .map(|v| v as u32),
            height: document
                .fields
                .get("height")
                .and_then(|v| v.as_f64())
                .map(|v| v as u32),
            focal_x: document.fields.get("focal_x").and_then(|v| v.as_f64()),
            focal_y: document.fields.get("focal_y").and_then(|v| v.as_f64()),
        }
    }
}

/// Build upload preview/info context for upload collection edit forms.
fn build_upload_context(def: &CollectionDefinition, document: &Document) -> Value {
    let mut ctx = json!({});

    if let Some(ref u) = def.upload
        && !u.mime_types.is_empty()
    {
        ctx["accept"] = json!(u.mime_types.join(","));
    }

    let meta = UploadMeta::from_document(document);

    if let Some(fx) = meta.focal_x {
        ctx["focal_x"] = json!(fx);
    }

    if let Some(fy) = meta.focal_y {
        ctx["focal_y"] = json!(fy);
    }

    if let (Some(url), Some(mime)) = (meta.url, meta.mime_type)
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

    if let Some(fname) = meta.filename {
        let mut info = json!({ "filename": fname });

        if let Some(size) = meta.filesize {
            info["filesize_display"] = json!(upload::format_filesize(size));
        }

        if let (Some(w), Some(h)) = (meta.width, meta.height) {
            info["dimensions"] = json!(format!("{}x{}", w, h));
        }

        ctx["info"] = info;
    }

    ctx
}

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

    let access_constraints = if let AccessResult::Constrained(ref filters) = access_result {
        Some(filters.clone())
    } else {
        None
    };

    let read_params = ReadParams {
        pool: state.pool.clone(),
        runner: state.hook_runner.clone(),
        hooks: def.hooks.clone(),
        fields: def.fields.clone(),
        slug: slug.clone(),
        id: id.clone(),
        def: def.clone(),
        locale_ctx,
        access_constraints,
        has_drafts: def.has_drafts(),
        user_doc: auth_user.as_ref().map(|Extension(au)| au.user_doc.clone()),
        user_ui_locale: auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone()),
    };

    let read_result = task::spawn_blocking(move || read_document(read_params)).await;

    let mut document = match read_result {
        Ok(Ok(Some(doc))) => doc,
        Ok(Ok(None)) => {
            return not_found(&state, &format!("Document '{}' not found", id));
        }
        Ok(Err(e)) => {
            error!("Document edit query error: {}", e);

            return server_error(&state, "An internal error occurred.");
        }
        Err(e) => {
            error!("Document edit task error: {}", e);

            return server_error(&state, "An internal error occurred.");
        }
    };

    // Strip field-level read-denied fields (fail closed on pool exhaustion)
    let denied = match compute_denied_read_fields(&state, &auth_user, &def.fields) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };
    strip_denied_fields(&mut document.fields, &denied);

    let (main_fields, sidebar_fields) =
        prepare_edit_fields(&state, &def, &document, &id, editor_locale.as_deref());

    let doc_title = def
        .title_field()
        .and_then(|f| document.get_str(f))
        .map(|s| s.to_string())
        .unwrap_or_else(|| document.id.to_string());

    let has_drafts = def.has_drafts();
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

    let (versions, total_versions) = if has_versions {
        fetch_version_sidebar_data(&state.pool, &slug, &document.id)
    } else {
        (vec![], 0)
    };

    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let mut builder = ContextBuilder::new(&state, claims_ref)
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
        .set("document_title", json!(doc_title))
        .set(
            "ref_count",
            json!(lookup_ref_count(&state.pool, &slug, &id)),
        )
        .breadcrumbs(vec![
            Breadcrumb::link("collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::current(doc_title.clone()),
        ])
        .merge(locale_data);

    if def.is_upload_collection() {
        builder = builder.set("upload", build_upload_context(&def, &document));
    }

    let data = builder.build();
    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/edit", &data)
}
