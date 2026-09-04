use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::{Value, json};

use crate::admin::handlers::shared::HxNav;
use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, Breadcrumb, GlobalContext, GlobalPermissions, PageMeta, PageType,
            field::FieldContext, page::globals::GlobalEditPage,
        },
        handlers::shared::{
            EnrichOptions, PageRequest, apply_display_conditions, build_field_contexts,
            build_locale_template_data, compute_denied_read_fields, enrich_field_contexts,
            extract_doc_status, extract_editor_locale, fetch_version_sidebar_data,
            flatten_document_values, get_user_doc, is_non_default_locale, paths, render_page,
            require_global, service_error_to_admin_response, split_sidebar_fields,
        },
    },
    core::{AuthUser, Claims, DocumentFields, FieldDenial, collection::GlobalDefinition},
    hooks::ConditionContext,
    service::{
        RunnerReadHooks, ServiceContext,
        op::{self, GetGlobal, GetGlobalArgs, Principal, TargetRef},
    },
};

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
        &state.infra.hook_runner,
        false,
        &cond_ctx,
    );

    split_sidebar_fields(fields)
}

/// GET /admin/globals/{slug} — show edit form for a global
/// Fetch the version-history sidebar data for a global, or `(vec![], 0)` when the
/// global has no versions feature or a DB connection can't be acquired.
fn fetch_global_version_sidebar(
    state: &AdminState,
    def: &GlobalDefinition,
    slug: &str,
    auth_user: Option<&Extension<AuthUser>>,
) -> (Vec<Value>, i64) {
    if !def.has_versions() {
        return (vec![], 0);
    }
    let Ok(vc) = state.infra.pool.get() else {
        return (vec![], 0);
    };

    let vh = RunnerReadHooks::new(
        &state.infra.hook_runner,
        &vc,
        auth_user.map(|Extension(au)| &au.user_doc),
        None,
    );
    let version_ctx = ServiceContext::global(slug, def)
        .conn(&vc)
        .read_hooks(&vh)
        .user(auth_user.map(|Extension(au)| &au.user_doc))
        .build();

    fetch_version_sidebar_data(&version_ctx, "default")
}

pub async fn edit_form(
    State(state): State<AdminState>,
    hx: HxNav,
    Path(slug): Path<String>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match require_global(&state, &slug) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let (locale_ctx, locale_data) = build_locale_template_data(&state, editor_locale.as_deref());

    // Opt into the draft overlay unconditionally — the service read downgrades
    // (never rejects): an editor sees the latest draft, a read-only viewer falls
    // back to the published row. `GlobalPermissions` is a UI hint only.
    let ui_locale = auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone());
    let args = GetGlobalArgs::builder()
        .locale_ctx(locale_ctx)
        .include_drafts(true)
        .build();

    let read_result = op::run_blocking::<GetGlobal>(
        Arc::clone(&state.infra),
        Principal::Resolved {
            user: auth_user.as_ref().map(|Extension(au)| au.user_doc.clone()),
            ui_locale,
        },
        TargetRef::global(slug.as_str()),
        args,
    )
    .await;

    let document = match read_result {
        Ok(doc) => doc,
        Err(e) => {
            return service_error_to_admin_response(
                &state,
                e.into_service_error(),
                "You don't have permission to view this global",
            );
        }
    };

    // The service read already stripped read-denied *values*; resolve the denied
    // field *names* for this document so the form can drop their inputs.
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

    let (versions, total_versions) =
        fetch_global_version_sidebar(&state, &def, &slug, auth_user.as_ref());

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

    render_page(
        &state,
        PageRequest::new(hx, auth_user.as_ref()),
        "globals/edit",
        &ctx,
    )
    .await
}
