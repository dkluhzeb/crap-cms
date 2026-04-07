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

use crate::{
    admin::{
        AdminState,
        context::{Breadcrumb, ContextBuilder, PageType},
        handlers::shared::{
            EnrichOptions, apply_display_conditions, build_field_contexts,
            build_locale_template_data, check_access_or_forbid, compute_denied_read_fields,
            enrich_field_contexts, extract_doc_status, extract_editor_locale,
            fetch_version_sidebar_data, flatten_document_values, forbidden, is_non_default_locale,
            not_found, render_or_error, server_error, split_sidebar_fields, strip_denied_fields,
        },
    },
    core::{
        Document,
        auth::{AuthUser, Claims},
        collection::GlobalDefinition,
    },
    db::{
        DbPool,
        query::{AccessResult, helpers::global_table},
    },
    hooks::HookRunner,
    service::{RunnerReadHooks, get_global_document},
};

/// Parameters for the blocking global-read task.
struct ReadParams {
    pool: DbPool,
    runner: HookRunner,
    slug: String,
    def: GlobalDefinition,
    locale_ctx: Option<crate::db::query::LocaleContext>,
    user_doc: Option<Document>,
    user_ui_locale: Option<String>,
}

/// Fetch the global document via the shared service layer read lifecycle.
fn read_global_document(params: ReadParams) -> Result<Document, anyhow::Error> {
    let conn = params.pool.get()?;

    let hooks = RunnerReadHooks {
        runner: &params.runner,
        conn: &conn,
    };

    get_global_document(
        &conn,
        &hooks,
        &params.slug,
        &params.def,
        params.locale_ctx.as_ref(),
        params.user_doc.as_ref(),
        params.user_ui_locale.as_deref(),
    )
}

/// Build, enrich, and split the field contexts for the global edit form.
fn prepare_edit_fields(
    state: &AdminState,
    def: &GlobalDefinition,
    doc_fields: &HashMap<String, Value>,
    editor_locale: Option<&str>,
) -> (Vec<Value>, Vec<Value>) {
    let values = flatten_document_values(doc_fields, &def.fields);
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
        doc_fields,
        state,
        &EnrichOptions::builder(&HashMap::new())
            .non_default_locale(non_default_locale)
            .build(),
    );

    let form_data_json = json!(doc_fields);
    apply_display_conditions(
        &mut fields,
        &def.fields,
        &form_data_json,
        &state.hook_runner,
        false,
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
        None => return not_found(&state, &format!("Global '{}' not found", slug)),
    };

    match check_access_or_forbid(&state, def.access.read.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to view this global");
        }
        Err(resp) => return *resp,
        _ => {}
    }

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let (locale_ctx, locale_data) = build_locale_template_data(&state, editor_locale.as_deref());

    let read_params = ReadParams {
        pool: state.pool.clone(),
        runner: state.hook_runner.clone(),
        slug: slug.clone(),
        def: def.clone(),
        locale_ctx,
        user_doc: auth_user.as_ref().map(|Extension(au)| au.user_doc.clone()),
        user_ui_locale: auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone()),
    };

    let read_result = task::spawn_blocking(move || read_global_document(read_params)).await;

    let document = match read_result {
        Ok(Ok(doc)) => doc,
        Ok(Err(e)) => {
            error!("Global read query error: {}", e);
            return server_error(&state, "An internal error occurred.");
        }
        Err(e) => {
            error!("Global read task error: {}", e);
            return server_error(&state, "An internal error occurred.");
        }
    };

    let mut doc_fields = document.fields.clone();
    let denied = match compute_denied_read_fields(&state, &auth_user, &def.fields) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };
    strip_denied_fields(&mut doc_fields, &denied);

    let (main_fields, sidebar_fields) =
        prepare_edit_fields(&state, &def, &doc_fields, editor_locale.as_deref());

    let has_versions = def.has_versions();
    let has_drafts = def.has_drafts();
    let doc_status = extract_doc_status(&document, has_drafts);

    let gtable = global_table(&slug);
    let (versions, total_versions) = if has_versions {
        fetch_version_sidebar_data(&state.pool, &gtable, "default")
    } else {
        (vec![], 0)
    };

    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .filter_nav_by_access(&state, &auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::GlobalEdit, def.display_name())
        .breadcrumbs(vec![
            Breadcrumb::link("dashboard", "/admin"),
            Breadcrumb::current(def.display_name()),
        ])
        .global_def(&def)
        .fields(main_fields)
        .set("sidebar_fields", json!(sidebar_fields))
        .set("has_drafts", json!(has_drafts))
        .set("has_versions", json!(has_versions))
        .set("versions", json!(versions))
        .set("has_more_versions", json!(total_versions > 3))
        .set(
            "restore_url_prefix",
            json!(format!("/admin/globals/{}", slug)),
        )
        .set(
            "versions_url",
            json!(format!("/admin/globals/{}/versions", slug)),
        )
        .set("doc_status", json!(doc_status))
        .merge(locale_data)
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "globals/edit", &data)
}
