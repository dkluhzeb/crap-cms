use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};

use serde_json::{Value, json};
use tracing::warn;

use crate::admin::context::field::{
    BaseFieldData, CheckboxField, ConditionData, FieldContext, TextField, ValidationAttrs,
};
use crate::admin::handlers::shared::HxNav;

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, Breadcrumb, CollectionContext, CollectionPermissions, DocumentRef,
            LocaleTemplateData, PageMeta, PageType,
            page::collections::{CollectionEditPage, UploadFormContext, UploadInfo},
        },
        handlers::shared::{
            EnrichOptions, apply_display_conditions, build_field_contexts,
            build_locale_template_data, collection_base, compute_denied_read_fields,
            enrich_field_contexts, extract_doc_status, extract_editor_locale,
            fetch_version_sidebar_data, flatten_document_values, get_user_doc,
            is_non_default_locale, lookup_ref_count, not_found, paths, render_page,
            require_collection, service_error_to_admin_response, split_sidebar_fields,
        },
    },
    core::{AuthUser, Claims, CollectionDefinition, Document, FieldDenial, upload},
    db::{DbPool, query::LocaleContext},
    hooks::ConditionContext,
    service::{
        RunnerReadHooks, ServiceContext, ServiceError,
        auth::is_locked,
        op::{self, FindById, FindByIdArgs, Principal, TargetRef},
    },
};

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
///
/// Fails closed: the update form treats an unchecked lock box as an explicit
/// unlock, so when the lock state cannot be read the render must error
/// instead of silently defaulting the checkbox to "unlocked".
fn append_auth_fields(
    fields: &mut Vec<FieldContext>,
    pool: &DbPool,
    slug: &str,
    id: &str,
) -> Result<(), ServiceError> {
    fields.push(FieldContext::Password(TextField {
        base: auth_field_base("password", "password", Some("leave_blank_keep_password")),
        has_many: None,
        tags: None,
    }));

    let conn = pool
        .get()
        .inspect_err(|e| warn!("Failed to read lock status for user {id}: {e}"))
        .map_err(ServiceError::Internal)?;

    let ctx = ServiceContext::slug_only(slug).conn(&conn).build();
    let locked = is_locked(&ctx, id)
        .inspect_err(|e| warn!("Failed to read lock status for user {id}: {e}"))?;

    fields.push(FieldContext::Checkbox(CheckboxField {
        base: auth_field_base("_locked", "account_locked", Some("prevent_login")),
        checked: locked,
    }));

    Ok(())
}

/// Build, enrich, and split the field contexts for the edit form.
fn prepare_edit_fields(
    state: &AdminState,
    def: &CollectionDefinition,
    document: &Document,
    id: &str,
    editor_locale: Option<&str>,
    denied_read_fields: &[FieldDenial],
    auth_user: Option<&Extension<AuthUser>>,
) -> Result<(Vec<FieldContext>, Vec<FieldContext>), ServiceError> {
    // The service read already stripped read-denied *values* (data-aware), so
    // this view of the data carries only readable values; `denied_read_fields`
    // is used below only to drop the denied fields' input contexts.
    let visible_fields = document.fields.clone();

    let values = flatten_document_values(&visible_fields, &def.fields);
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
        &visible_fields,
        state,
        &EnrichOptions::builder(&HashMap::new())
            .filter_hidden(true)
            .non_default_locale(non_default_locale)
            .doc_id(Some(id))
            .user(get_user_doc(auth_user))
            .build(),
    );

    // Drop top-level denied field contexts so no empty input renders for them.
    if !denied_read_fields.is_empty() {
        fields.retain(|fc| {
            let name = fc.base().name.as_str();
            !denied_read_fields.iter().any(|d| d.display_path() == name)
        });
    }

    let form_data_json = json!(visible_fields);
    let cond_ctx = ConditionContext {
        collection: &def.slug,
        operation: "update",
        user: get_user_doc(auth_user),
        ui_locale: auth_user.map(|Extension(au)| au.ui_locale.as_str()),
        locale: editor_locale,
        options: None,
    };
    apply_display_conditions(
        &mut fields,
        &def.fields,
        &form_data_json,
        &state.infra.hook_runner,
        true,
        &cond_ctx,
    );

    if def.is_auth_collection() {
        append_auth_fields(&mut fields, &state.infra.pool, &def.slug, id)?;
    }

    Ok(split_sidebar_fields(fields))
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
                .and_then(serde_json::Value::as_u64),
            width: document
                .fields
                .get("width")
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u32::try_from(v).ok()),
            height: document
                .fields
                .get("height")
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u32::try_from(v).ok()),
            focal_x: document
                .fields
                .get("focal_x")
                .and_then(serde_json::Value::as_f64),
            focal_y: document
                .fields
                .get("focal_y")
                .and_then(serde_json::Value::as_f64),
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
                    .map(std::string::ToString::to_string)
            })
            .unwrap_or_else(|| url.to_string());

        ctx.preview = Some(preview_url);
    }

    if let Some(fname) = meta.filename {
        let dimensions = match (meta.width, meta.height) {
            (Some(w), Some(h)) => Some(format!("{w}x{h}")),
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

/// GET /admin/collections/{slug}/{id} — show edit form.
///
/// Thin orchestrator (per CLAUDE.md): resolve def → load document via
/// `spawn_blocking` → compute denied fields → prepare main+sidebar field
/// contexts → assemble the page context and render. Per-phase logic
/// lives in the helpers above.
pub async fn edit_form(
    State(state): State<AdminState>,
    hx: HxNav,
    Path((slug, id)): Path<(String, String)>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match require_collection(&state, &slug) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let (locale_ctx, locale_data) = build_locale_template_data(&state, editor_locale.as_deref());

    let document = match load_document(&state, &slug, &id, locale_ctx, auth_user.as_ref()).await {
        Ok(doc) => doc,
        Err(resp) => return resp,
    };

    // The service read already stripped read-denied *values* (data-aware). Here
    // we resolve the denied field *names* for this document so the form can drop
    // their inputs (no empty input renders for a field the user can't read).
    let denied = match compute_denied_read_fields(
        &state,
        auth_user.as_ref(),
        &def.fields,
        &slug,
        &document.fields,
    ) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    let (main_fields, sidebar_fields) = match prepare_edit_fields(
        &state,
        &def,
        &document,
        &id,
        editor_locale.as_deref(),
        &denied,
        auth_user.as_ref(),
    ) {
        Ok(fields) => fields,
        Err(e) => {
            return service_error_to_admin_response(
                &state,
                e,
                "You don't have permission to view this item",
            );
        }
    };

    let ctx = build_edit_page_context(EditPageContextInput {
        state: &state,
        slug: &slug,
        id: &id,
        def: &def,
        document: &document,
        claims: claims.as_ref(),
        auth_user: auth_user.as_ref(),
        editor_locale: editor_locale.as_deref(),
        locale_data,
        main_fields,
        sidebar_fields,
    });

    render_page(&state, hx, "collections/edit", &ctx)
}

/// Run the blocking document read on a tokio task and fold the
/// nested Result/Option into a single page-level outcome — either
/// the loaded document, or the error response that the handler should
/// return verbatim.
async fn load_document(
    state: &AdminState,
    slug: &str,
    id: &str,
    locale_ctx: Option<LocaleContext>,
    auth_user: Option<&Extension<AuthUser>>,
) -> Result<Document, Response> {
    // Opt into the draft overlay unconditionally — the service read downgrades
    // (never rejects): an editor sees the latest draft, a read-only viewer falls
    // back to the published version. `CollectionPermissions` is a UI hint only.
    // The middleware already resolved the session, so the principal is
    // pre-resolved rather than re-running the evaluator per operation.
    let args = FindByIdArgs::builder(id)
        .locale_ctx(locale_ctx)
        .use_draft(true)
        .build();

    let read_result = op::run_blocking::<FindById>(
        Arc::clone(&state.infra),
        Principal::Resolved {
            user: auth_user.map(|Extension(au)| au.user_doc.clone()),
            ui_locale: auth_user.map(|Extension(au)| au.ui_locale.clone()),
        },
        TargetRef::collection(slug),
        args,
    )
    .await;

    match read_result {
        Ok(Some(doc)) => Ok(doc),
        Ok(None) => Err(not_found(state, &format!("Document '{id}' not found"))),
        Err(e) => Err(service_error_to_admin_response(
            state,
            e.into_service_error(),
            "You don't have permission to view this item",
        )),
    }
}

/// Fetch the version-sidebar items + total version count for a
/// versioned collection. Returns `(vec![], 0)` when versioning is off
/// or the connection pool is exhausted (best-effort sidebar; the rest
/// of the page still renders).
fn fetch_versions_for_sidebar(
    state: &AdminState,
    slug: &str,
    def: &CollectionDefinition,
    doc_id: &str,
    user: Option<&Document>,
) -> (Vec<Value>, i64) {
    if !def.has_versions() {
        return (vec![], 0);
    }
    let Ok(vc) = state.infra.pool.get() else {
        return (vec![], 0);
    };
    let vh = RunnerReadHooks::new(&state.infra.hook_runner, &vc, user, None);
    let version_ctx = ServiceContext::collection(slug, def)
        .conn(&vc)
        .read_hooks(&vh)
        .user(user)
        .build();
    fetch_version_sidebar_data(&version_ctx, doc_id)
}

/// Bundled inputs for [`build_edit_page_context`] — keeps the call
/// site readable instead of a 10-arg positional call.
struct EditPageContextInput<'a> {
    state: &'a AdminState,
    slug: &'a str,
    id: &'a str,
    def: &'a CollectionDefinition,
    document: &'a Document,
    claims: Option<&'a Extension<Claims>>,
    auth_user: Option<&'a Extension<AuthUser>>,
    editor_locale: Option<&'a str>,
    locale_data: Option<LocaleTemplateData>,
    main_fields: Vec<FieldContext>,
    sidebar_fields: Vec<FieldContext>,
}

/// Compose the [`CollectionEditPage`] view-model from the loaded
/// document + prepared field contexts. All the "wrap presentation
/// state into a typed page context" work lives here.
fn build_edit_page_context(input: EditPageContextInput<'_>) -> CollectionEditPage {
    let doc_title = input
        .def
        .title_field()
        .and_then(|f| input.document.get_str(f))
        .map_or_else(
            || input.document.id.to_string(),
            std::string::ToString::to_string,
        );

    let has_drafts = input.def.has_drafts();
    let has_versions = input.def.has_versions();
    let doc_status = extract_doc_status(input.document, has_drafts);

    let (versions, total_versions) = fetch_versions_for_sidebar(
        input.state,
        input.slug,
        input.def,
        &input.document.id,
        input.auth_user.map(|Extension(au)| &au.user_doc),
    );

    let claims_ref = input.claims.map(|Extension(c)| c);

    let mut breadcrumbs = collection_base(input.def, input.slug);
    breadcrumbs.push(Breadcrumb::current(doc_title.clone()));

    let base = BasePageContext::for_handler(
        input.state,
        claims_ref,
        input.auth_user,
        PageMeta::new(PageType::CollectionEdit, "edit_name")
            .with_title_name(input.def.singular_name()),
    )
    .with_editor_locale(input.editor_locale, input.state)
    .with_breadcrumbs(breadcrumbs);

    let upload = input
        .def
        .is_upload_collection()
        .then(|| build_upload_context(input.def, input.document));

    let perms = CollectionPermissions::for_user(input.state, input.def, input.auth_user);

    CollectionEditPage {
        base,
        collection: CollectionContext::from_def(input.def),
        perms,
        document: DocumentRef::with_status(input.document, &doc_status),
        fields: input.main_fields,
        sidebar_fields: input.sidebar_fields,
        editing: true,
        has_drafts,
        has_versions,
        versions,
        has_more_versions: total_versions > 3,
        restore_url_prefix: paths::collection_item(input.slug, input.id),
        versions_url: paths::collection_item_versions(input.slug, input.id),
        document_title: doc_title,
        ref_count: lookup_ref_count(&input.state.infra.pool, input.slug, input.id),
        locale_data: input.locale_data,
        upload,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use r2d2_sqlite::SqliteConnectionManager;
    use serde_json::{Value, json};

    use crate::core::document::DocumentBuilder;
    use crate::core::upload::CollectionUpload;

    use super::*;

    #[test]
    fn append_auth_fields_fails_closed_when_lock_state_unreadable() {
        // Regression: a DB/pool error used to render the lock checkbox
        // unchecked (fail-open); saving that form would then explicitly
        // unlock the account. An unreadable lock state must error instead.
        let manager = SqliteConnectionManager::file("/nonexistent-dir/no.db");
        let pool = DbPool::from_pool(
            r2d2::Pool::builder()
                .max_size(1)
                .connection_timeout(Duration::from_millis(100))
                .build_unchecked(manager),
        );

        let mut fields = Vec::new();
        let result = append_auth_fields(&mut fields, &pool, "users", "u1");

        assert!(
            result.is_err(),
            "unreadable lock state must fail the render, not default to unlocked"
        );
    }

    fn media_def(thumb: Option<&str>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("media");
        def.upload = Some(CollectionUpload {
            mime_types: vec!["image/png".into()],
            admin_thumbnail: thumb.map(str::to_string),
            ..Default::default()
        });
        def
    }

    fn doc(fields: Value) -> Document {
        let map: HashMap<String, Value> = serde_json::from_value(fields).unwrap();
        DocumentBuilder::new("u1").fields(map).build()
    }

    #[test]
    fn image_with_thumbnail_uses_sized_preview_and_formats_dimensions() {
        let def = media_def(Some("card"));
        let d = doc(json!({
            "url": "/orig.png", "mime_type": "image/png", "filename": "p.png",
            "filesize": 2048, "width": 800, "height": 600,
            "focal_x": 0.5, "focal_y": 0.25,
            "sizes": { "card": { "url": "/card.png" } }
        }));

        let ctx = build_upload_context(&def, &d);

        assert_eq!(ctx.accept.as_deref(), Some("image/png"));
        assert_eq!(ctx.focal_x, Some(0.5));
        assert_eq!(ctx.focal_y, Some(0.25));
        assert_eq!(ctx.preview.as_deref(), Some("/card.png"));

        let info = ctx.info.expect("info present when filename set");
        assert_eq!(info.filename, "p.png");
        assert_eq!(info.dimensions.as_deref(), Some("800x600"));
        assert!(info.filesize_display.is_some());
    }

    #[test]
    fn image_without_thumbnail_falls_back_to_original_url() {
        let def = media_def(None);
        let d = doc(json!({ "url": "/orig.png", "mime_type": "image/jpeg", "filename": "p.jpg" }));

        let ctx = build_upload_context(&def, &d);
        assert_eq!(ctx.preview.as_deref(), Some("/orig.png"));
        // No width/height → no dimensions string.
        assert_eq!(ctx.info.expect("info").dimensions, None);
    }

    #[test]
    fn non_image_has_no_preview() {
        let def = media_def(Some("card"));
        let d =
            doc(json!({ "url": "/f.pdf", "mime_type": "application/pdf", "filename": "f.pdf" }));

        let ctx = build_upload_context(&def, &d);
        assert!(ctx.preview.is_none());
    }
}
