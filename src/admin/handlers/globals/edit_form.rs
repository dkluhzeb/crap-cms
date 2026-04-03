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
            enrich_field_contexts, extract_editor_locale, fetch_version_sidebar_data,
            flatten_document_values, forbidden, is_non_default_locale, not_found, render_or_error,
            server_error, split_sidebar_fields, strip_denied_fields,
        },
    },
    core::auth::{AuthUser, Claims},
    db::{ops, query::AccessResult},
};

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

    // Check read access
    match check_access_or_forbid(&state, def.access.read.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to view this global");
        }
        Err(resp) => return *resp,
        _ => {}
    }

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let (locale_ctx, locale_data) = build_locale_template_data(&state, editor_locale.as_deref());

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let hooks = def.hooks.clone();
    let fields = def.fields.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let user_doc = auth_user.as_ref().map(|Extension(au)| au.user_doc.clone());
    let user_ui_locale = auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone());

    let read_result = task::spawn_blocking(move || {
        runner.fire_before_read(&hooks, &slug_owned, "get_global", HashMap::new())?;
        let doc = ops::get_global(&pool, &slug_owned, &def_owned, locale_ctx.as_ref())?;
        let ar_ctx = crate::hooks::lifecycle::AfterReadCtx {
            hooks: &hooks,
            fields: &fields,
            collection: &slug_owned,
            operation: "get_global",
            user: user_doc.as_ref(),
            ui_locale: user_ui_locale.as_deref(),
        };
        let doc = runner.apply_after_read(&ar_ctx, doc);

        Ok::<_, anyhow::Error>(doc)
    })
    .await;

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

    // Strip field-level read-denied fields
    let mut doc_fields = document.fields.clone();
    let denied = match compute_denied_read_fields(&state, &auth_user, &def.fields) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };
    strip_denied_fields(&mut doc_fields, &denied);

    let values = flatten_document_values(&doc_fields, &def.fields);

    let non_default_locale = is_non_default_locale(&state, editor_locale.as_deref());

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
        &doc_fields,
        &state,
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

    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    let has_versions = def.has_versions();
    let has_drafts = def.has_drafts();

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

    let global_table = format!("_global_{}", slug);
    let (versions, total_versions): (Vec<Value>, i64) = if has_versions {
        fetch_version_sidebar_data(&state.pool, &global_table, "default")
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
