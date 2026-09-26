use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::Value;

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, Breadcrumb, GlobalContext, GlobalPermissions, PageMeta, PageType,
            field::FieldContext, page::globals::GlobalEditPage,
        },
        handlers::shared::{
            EnrichOptions, FormReadDenials, HxNav, PageRequest, apply_display_conditions,
            build_field_contexts, compute_denied_read_fields, condition_data, editor_locale_ctx,
            editor_read_ctx, enrich_field_contexts, extract_doc_status, extract_editor_locale,
            fetch_version_sidebar_data, flatten_document_values, get_user_doc,
            is_non_default_locale, paths, readable_form_fields, render_page, require_global,
            service_error_to_admin_response, split_sidebar_fields,
        },
    },
    core::{AuthUser, Claims, DocumentFields, collection::GlobalDefinition},
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
    denied_read_fields: &FormReadDenials,
    auth_user: Option<&Extension<AuthUser>>,
) -> (Vec<FieldContext>, Vec<FieldContext>) {
    // The service read (`get_global_document`) already stripped read-denied
    // *values* (data-aware); the form renders no input at all for a field the
    // viewer may not read, at any depth.
    let visible_fields = doc_fields.clone();
    let form_fields = readable_form_fields(&def.fields, &denied_read_fields.flat);

    let values = flatten_document_values(&visible_fields, &form_fields);
    let non_default_locale = is_non_default_locale(state, editor_locale);

    // `admin.hidden` means "not in the admin form, value kept" — the same
    // promise the collection forms make, and the one the submit-side
    // normalizers rely on when they read an absent key as an edit.
    let mut fields = build_field_contexts(
        &form_fields,
        &values,
        &HashMap::new(),
        true,
        non_default_locale,
    );

    let enrich_locale_ctx = editor_locale_ctx(&state.config.locale, editor_locale);
    enrich_field_contexts(
        &mut fields,
        &form_fields,
        &visible_fields,
        state,
        &EnrichOptions::builder(&HashMap::new())
            .filter_hidden(true)
            .non_default_locale(non_default_locale)
            .user(get_user_doc(auth_user))
            .locale_ctx(enrich_locale_ctx.as_ref())
            .build(),
    );

    // Each row renders the sub-fields its viewer may read in that row.
    denied_read_fields.rows.prune(&mut fields);

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
        &form_fields,
        &condition_data(&def.fields, &visible_fields),
        &state.infra.hook_runner,
        true,
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
    let locale_ctx = editor_read_ctx(&state, editor_locale.as_deref());

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
        revision: document.revision(),
    };

    render_page(
        &state,
        PageRequest::new(hx, auth_user.as_ref()),
        "globals/edit",
        &ctx,
    )
    .await
}
