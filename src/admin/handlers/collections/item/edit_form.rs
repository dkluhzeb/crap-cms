use std::collections::HashMap;

use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};

use serde_json::{Value, json};
use tokio::task;
use tracing::error;

use crate::admin::context::field::{
    BaseFieldData, CheckboxField, ConditionData, FieldContext, TextField, ValidationAttrs,
};

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, Breadcrumb, CollectionContext, DocumentRef, PageMeta, PageType,
            page::collections::{CollectionEditPage, UploadFormContext, UploadInfo},
        },
        handlers::shared::{
            EnrichOptions, apply_display_conditions, build_field_contexts,
            build_locale_template_data, compute_denied_read_fields, enrich_field_contexts,
            extract_doc_status, extract_editor_locale, fetch_version_sidebar_data,
            flatten_document_values, forbidden, is_non_default_locale, lookup_ref_count, not_found,
            paths, render_page, server_error, split_sidebar_fields,
        },
    },
    core::{
        CollectionDefinition, Document,
        auth::{AuthUser, Claims},
        upload,
    },
    db::{DbPool, query::LocaleContext},
    hooks::HookRunner,
    service::{
        FindByIdInput, RunnerReadHooks, ServiceContext, ServiceError, auth::is_locked,
        find_document_by_id,
    },
};

/// Parameters for the blocking document-read task.
struct ReadParams {
    pool: DbPool,
    runner: HookRunner,
    slug: String,
    id: String,
    def: CollectionDefinition,
    locale_ctx: Option<LocaleContext>,
    has_drafts: bool,
    user_doc: Option<Document>,
}

/// Fetch the document via the shared service layer read lifecycle.
fn read_document(params: ReadParams) -> Result<Option<Document>, ServiceError> {
    let conn = params.pool.get().map_err(ServiceError::Internal)?;

    let hooks = RunnerReadHooks::new(&params.runner, &conn);
    let ctx = ServiceContext::collection(&params.slug, &params.def)
        .pool(&params.pool)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(params.user_doc.as_ref())
        .build();

    let input = FindByIdInput::builder(&params.id)
        .use_draft(params.has_drafts)
        .locale_ctx(params.locale_ctx.as_ref())
        .build();

    find_document_by_id(&ctx, &input)
}

/// Build a synthetic [`BaseFieldData`] for an auth-only injected field.
fn auth_field_base(name: &str, label: &str, description: Option<&str>) -> BaseFieldData {
    BaseFieldData {
        name: name.to_string(),
        field_name: name.to_string(),
        label: label.to_string(),
        required: false,
        value: Value::String(String::new()),
        placeholder: None,
        description: description.map(str::to_string),
        readonly: false,
        localized: false,
        locale_locked: false,
        position: None,
        template: None,
        extra: serde_json::Map::new(),
        error: None,
        validation: ValidationAttrs::default(),
        condition: ConditionData::default(),
    }
}

/// Append auth-specific fields (password, locked checkbox) to the field list.
fn append_auth_fields(fields: &mut Vec<FieldContext>, pool: &DbPool, slug: &str, id: &str) {
    fields.push(FieldContext::Password(TextField {
        base: auth_field_base("password", "password", Some("leave_blank_keep_password")),
        has_many: None,
        tags: None,
    }));

    let is_locked = pool
        .get()
        .ok()
        .and_then(|conn| {
            let ctx = ServiceContext::slug_only(slug).conn(&conn).build();
            is_locked(&ctx, id).ok()
        })
        .unwrap_or(false);

    fields.push(FieldContext::Checkbox(CheckboxField {
        base: auth_field_base("_locked", "account_locked", Some("prevent_login")),
        checked: is_locked,
    }));
}

/// Build, enrich, and split the field contexts for the edit form.
fn prepare_edit_fields(
    state: &AdminState,
    def: &CollectionDefinition,
    document: &Document,
    id: &str,
    editor_locale: Option<&str>,
    denied_read_fields: &[String],
) -> (Vec<FieldContext>, Vec<FieldContext>) {
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

    // Remove read-denied fields entirely — they shouldn't render in the form
    if !denied_read_fields.is_empty() {
        fields.retain(|fc| {
            let name = fc.base().name.as_str();
            !denied_read_fields.iter().any(|d| d == name)
        });
    }

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
fn build_upload_context(def: &CollectionDefinition, document: &Document) -> UploadFormContext {
    let mut ctx = UploadFormContext::default();

    if let Some(ref u) = def.upload
        && !u.mime_types.is_empty()
    {
        ctx.accept = Some(u.mime_types.join(","));
    }

    let meta = UploadMeta::from_document(document);
    ctx.focal_x = meta.focal_x;
    ctx.focal_y = meta.focal_y;

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

        ctx.preview = Some(preview_url);
    }

    if let Some(fname) = meta.filename {
        let dimensions = match (meta.width, meta.height) {
            (Some(w), Some(h)) => Some(format!("{}x{}", w, h)),
            _ => None,
        };

        ctx.info = Some(UploadInfo {
            filename: fname.to_string(),
            filesize_display: meta.filesize.map(upload::format_filesize),
            dimensions,
        });
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

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let (locale_ctx, locale_data) = build_locale_template_data(&state, editor_locale.as_deref());

    let read_params = ReadParams {
        pool: state.pool.clone(),
        runner: state.hook_runner.clone(),
        slug: slug.clone(),
        id: id.clone(),
        def: def.clone(),
        locale_ctx,
        has_drafts: def.has_drafts(),
        user_doc: auth_user.as_ref().map(|Extension(au)| au.user_doc.clone()),
    };

    let read_result = task::spawn_blocking(move || read_document(read_params)).await;

    let document = match read_result {
        Ok(Ok(Some(doc))) => doc,
        Ok(Ok(None)) => {
            return not_found(&state, &format!("Document '{}' not found", id));
        }
        Ok(Err(ServiceError::AccessDenied(_))) => {
            return forbidden(&state, "You don't have permission to view this item");
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

    // Compute read-denied fields to exclude from form rendering.
    // The service already stripped their values — this filters the form fields themselves.
    let denied = match compute_denied_read_fields(&state, &auth_user, &def.fields) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    let (main_fields, sidebar_fields) = prepare_edit_fields(
        &state,
        &def,
        &document,
        &id,
        editor_locale.as_deref(),
        &denied,
    );

    let doc_title = def
        .title_field()
        .and_then(|f| document.get_str(f))
        .map(|s| s.to_string())
        .unwrap_or_else(|| document.id.to_string());

    let has_drafts = def.has_drafts();
    let has_versions = def.has_versions();

    let doc_status = extract_doc_status(&document, has_drafts);

    let (versions, total_versions) = if has_versions {
        if let Ok(vc) = state.pool.get() {
            let vh = RunnerReadHooks::new(&state.hook_runner, &vc);
            let version_ctx = ServiceContext::collection(&slug, &def)
                .conn(&vc)
                .read_hooks(&vh)
                .build();
            fetch_version_sidebar_data(&version_ctx, &document.id)
        } else {
            (vec![], 0)
        }
    } else {
        (vec![], 0)
    };

    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let breadcrumbs = vec![
        Breadcrumb::link("collections", "/admin/collections"),
        Breadcrumb::link(def.display_name(), paths::collection(&slug)),
        Breadcrumb::current(doc_title.clone()),
    ];

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        &auth_user,
        PageMeta::new(PageType::CollectionEdit, "edit_name").with_title_name(def.singular_name()),
    )
    .with_editor_locale(editor_locale.as_deref(), &state)
    .with_breadcrumbs(breadcrumbs);

    let upload = def
        .is_upload_collection()
        .then(|| build_upload_context(&def, &document));

    let ctx = CollectionEditPage {
        base,
        collection: CollectionContext::from_def(&def),
        document: DocumentRef::with_status(&document, &doc_status),
        fields: main_fields,
        sidebar_fields,
        editing: true,
        has_drafts,
        has_versions,
        versions,
        has_more_versions: total_versions > 3,
        restore_url_prefix: paths::collection_item(&slug, &id),
        versions_url: paths::collection_item_versions(&slug, &id),
        document_title: doc_title,
        ref_count: lookup_ref_count(&state.pool, &slug, &id),
        locale_data,
        upload,
    };

    render_page(&state, "collections/edit", &ctx)
}
