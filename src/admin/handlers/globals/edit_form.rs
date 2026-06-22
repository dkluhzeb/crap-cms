use std::collections::HashMap;

use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::json;
use tokio::task;

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, Breadcrumb, GlobalContext, GlobalPermissions, PageMeta, PageType,
            field::FieldContext, page::globals::GlobalEditPage,
        },
        handlers::shared::{
            EnrichOptions, apply_display_conditions, build_field_contexts,
            build_locale_template_data, compute_denied_read_fields, enrich_field_contexts,
            extract_doc_status, extract_editor_locale, fetch_version_sidebar_data,
            flatten_document_values, get_user_doc, is_non_default_locale, not_found, paths,
            render_page, service_error_to_admin_response, split_sidebar_fields,
            task_join_error_response,
        },
    },
    core::{AuthUser, Claims, Document, DocumentFields, FieldDenial, collection::GlobalDefinition},
    db::DbPool,
    hooks::{ConditionContext, HookRunner},
    service::{GetGlobalInput, RunnerReadHooks, ServiceContext, ServiceError, get_global_document},
};

/// Parameters for the blocking global-read task.
struct ReadParams {
    pool: DbPool,
    runner: HookRunner,
    slug: String,
    def: GlobalDefinition,
    locale_ctx: Option<crate::db::LocaleContext>,
    user_doc: Option<Document>,
    user_ui_locale: Option<String>,
    /// Whether to surface the global's draft — true only when the user can view
    /// drafts (edit-level access). A read-only viewer sees published content.
    include_drafts: bool,
}

/// Fetch the global document via the shared service layer read lifecycle.
fn read_global_document_blocking(params: &ReadParams) -> Result<Document, ServiceError> {
    let conn = params.pool.get().map_err(ServiceError::Internal)?;

    let hooks = RunnerReadHooks::new(
        &params.runner,
        &conn,
        params.user_doc.as_ref(),
        params.user_ui_locale.as_deref(),
    );
    let ctx = ServiceContext::global(&params.slug, &params.def)
        .pool(&params.pool)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(params.user_doc.as_ref())
        .build();

    // Surface the unpublished global only for a user who can view drafts
    // (edit-level access); a read-only viewer sees the published content.
    let input = GetGlobalInput::new(params.locale_ctx.as_ref(), params.user_ui_locale.as_deref())
        .include_drafts(params.include_drafts);

    get_global_document(&ctx, &input)
}

/// Build, enrich, and split the field contexts for the global edit form.
fn prepare_edit_fields(
    state: &AdminState,
    def: &GlobalDefinition,
    doc_fields: &DocumentFields,
    editor_locale: Option<&str>,
    denied_read_fields: &[FieldDenial],
    auth_user: Option<&Extension<AuthUser>>,
) -> (Vec<FieldContext>, Vec<FieldContext>) {
    // The service read (`get_global_document`) already stripped read-denied
    // *values* (data-aware); `denied_read_fields` is used below only to drop the
    // denied fields' input contexts.
    let visible_fields = doc_fields.clone();

    let values = flatten_document_values(&visible_fields, &def.fields);
    let non_default_locale = is_non_default_locale(state, editor_locale);

    let mut fields = build_field_contexts(
        &def.fields,
        &values,
        &HashMap::new(),
        false,
        non_default_locale,
    );

    enrich_field_contexts(
        &mut fields,
        &def.fields,
        &visible_fields,
        state,
        &EnrichOptions::builder(&HashMap::new())
            .non_default_locale(non_default_locale)
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
        &state.hook_runner,
        false,
        &cond_ctx,
    );

    split_sidebar_fields(fields)
}

/// GET /admin/globals/{slug} — show edit form for a global
pub async fn edit_form(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return not_found(&state, &format!("Global '{slug}' not found")),
    };

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let (locale_ctx, locale_data) = build_locale_template_data(&state, editor_locale.as_deref());

    // Opt into the draft overlay unconditionally — the service read downgrades
    // (never rejects): an editor sees the latest draft, a read-only viewer falls
    // back to the published row. `GlobalPermissions` is a UI hint only.
    let read_params = ReadParams {
        pool: state.pool.clone(),
        runner: state.hook_runner.clone(),
        slug: slug.clone(),
        def: def.clone(),
        locale_ctx,
        user_doc: auth_user.as_ref().map(|Extension(au)| au.user_doc.clone()),
        user_ui_locale: auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone()),
        include_drafts: true,
    };

    let read_result =
        task::spawn_blocking(move || read_global_document_blocking(&read_params)).await;

    let document = match read_result {
        Ok(Ok(doc)) => doc,
        Ok(Err(e)) => {
            return service_error_to_admin_response(
                &state,
                e,
                "You don't have permission to view this global",
            );
        }
        Err(e) => return task_join_error_response(&state, &e),
    };

    // The service read already stripped read-denied *values*; resolve the denied
    // field *names* for this document so the form can drop their inputs.
    let denied =
        match compute_denied_read_fields(&state, auth_user.as_ref(), &def.fields, &document.fields)
        {
            Ok(d) => d,
            Err(resp) => return *resp,
        };

    let (main_fields, sidebar_fields) = prepare_edit_fields(
        &state,
        &def,
        &document.fields,
        editor_locale.as_deref(),
        &denied,
        auth_user.as_ref(),
    );

    let has_versions = def.has_versions();
    let has_drafts = def.has_drafts();
    let doc_status = extract_doc_status(&document, has_drafts);

    let (versions, total_versions) = if has_versions {
        if let Ok(vc) = state.pool.get() {
            let vh = RunnerReadHooks::new(
                &state.hook_runner,
                &vc,
                auth_user.as_ref().map(|Extension(au)| &au.user_doc),
                None,
            );
            let version_ctx = ServiceContext::global(&slug, &def)
                .conn(&vc)
                .read_hooks(&vh)
                .build();
            fetch_version_sidebar_data(&version_ctx, "default")
        } else {
            (vec![], 0)
        }
    } else {
        (vec![], 0)
    };

    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let breadcrumbs = vec![
        Breadcrumb::link("dashboard", paths::DASHBOARD),
        Breadcrumb::current(def.display_name()),
    ];

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        auth_user.as_ref(),
        PageMeta::new(PageType::GlobalEdit, def.display_name()),
    )
    .with_editor_locale(editor_locale.as_deref(), &state)
    .with_breadcrumbs(breadcrumbs);

    let perms = GlobalPermissions::for_user(&state, &def, auth_user.as_ref());

    let ctx = GlobalEditPage {
        base,
        global: GlobalContext::from_def(&def),
        perms,
        fields: main_fields,
        sidebar_fields,
        has_drafts,
        has_versions,
        versions,
        has_more_versions: total_versions > 3,
        restore_url_prefix: paths::global(&slug),
        versions_url: paths::global_versions(&slug),
        doc_status,
        locale_data,
    };

    render_page(&state, "globals/edit", &ctx)
}
