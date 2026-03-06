//! Global edit and update handlers.

use axum::{
    extract::{Form, Path, Query, State},
    response::IntoResponse,
    Extension,
};
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::admin::context::{ContextBuilder, PageType, Breadcrumb};
use crate::admin::handlers::collections::forms::{extract_join_data_from_form, transform_select_has_many};
use crate::core::auth::{AuthUser, Claims};
use crate::core::validate::ValidationError;
use crate::db::{ops, query};
use crate::db::query::{AccessResult, LocaleContext};

use super::shared::{
    PaginationParams, LocaleParams,
    get_user_doc, get_event_user,
    check_access_or_forbid, build_locale_template_data,
    is_non_default_locale,
    build_field_contexts, enrich_field_contexts,
    apply_display_conditions, split_sidebar_fields,
    version_to_json, fetch_version_sidebar_data, do_unpublish,
    forbidden, redirect_response, htmx_redirect, html_with_toast,
    render_or_error, not_found, server_error,
};

/// GET /admin/globals/{slug} — show edit form for a global
pub async fn edit_form(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Query(locale_params): Query<LocaleParams>,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return not_found(&state, &format!("Global '{}' not found", slug)).into_response(),
    };

    // Check read access
    match check_access_or_forbid(&state, def.access.read.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to view this global").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    let (locale_ctx, locale_data) = build_locale_template_data(&state, locale_params.locale.as_deref());

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let hooks = def.hooks.clone();
    let fields = def.fields.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let read_result = tokio::task::spawn_blocking(move || {
        runner.fire_before_read(&hooks, &slug_owned, "get_global", HashMap::new())?;
        let doc = ops::get_global(&pool, &slug_owned, &def_owned, locale_ctx.as_ref())?;
        let doc = runner.apply_after_read(&hooks, &fields, &slug_owned, "get_global", doc);
        Ok::<_, anyhow::Error>(doc)
    }).await;

    let document = match read_result {
        Ok(Ok(doc)) => doc,
        Ok(Err(e)) => return server_error(&state, &format!("Query error: {}", e)).into_response(),
        Err(e) => return server_error(&state, &format!("Task error: {}", e)).into_response(),
    };

    // Strip field-level read-denied fields (skip pool.get if no field-level access configured)
    let mut doc_fields = document.fields.clone();
    if def.fields.iter().any(|f| f.access.read.is_some()) {
        let user_doc = get_user_doc(&auth_user);
        if let Ok(conn) = state.pool.get() {
            let denied = state.hook_runner.check_field_read_access(&def.fields, user_doc, &conn);
            for name in &denied {
                doc_fields.remove(name);
            }
        }
    }

    let values: HashMap<String, String> = doc_fields.iter()
        .map(|(k, v)| {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            (k.clone(), s)
        })
        .collect();

    let non_default_locale = is_non_default_locale(&state, locale_params.locale.as_deref());
    let mut fields = build_field_contexts(&def.fields, &values, &HashMap::new(), false, non_default_locale);

    // Enrich relationship fields with options
    enrich_field_contexts(&mut fields, &def.fields, &doc_fields, &state, false, non_default_locale, &HashMap::new(), None);

    // Evaluate display conditions
    let form_data_json = serde_json::json!(doc_fields);
    apply_display_conditions(&mut fields, &def.fields, &form_data_json, &state.hook_runner, false);

    // Split fields into main and sidebar
    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    // Fetch document status and version history for versioned globals
    let has_versions = def.has_versions();
    let has_drafts = def.has_drafts();
    let doc_status = if has_drafts {
        document.fields.get("_status")
            .and_then(|v| v.as_str())
            .unwrap_or("published")
            .to_string()
    } else {
        String::new()
    };
    let global_table = format!("_global_{}", slug);
    let (versions, total_versions): (Vec<serde_json::Value>, i64) = if has_versions {
        fetch_version_sidebar_data(&state.pool, &global_table, "default")
    } else {
        (vec![], 0)
    };

    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .page(PageType::GlobalEdit, def.display_name())
        .breadcrumbs(vec![
            Breadcrumb::link("Dashboard", "/admin"),
            Breadcrumb::current(def.display_name()),
        ])
        .global_def(&def)
        .fields(main_fields)
        .set("sidebar_fields", serde_json::json!(sidebar_fields))
        .set("has_drafts", serde_json::json!(has_drafts))
        .set("has_versions", serde_json::json!(has_versions))
        .set("versions", serde_json::json!(versions))
        .set("has_more_versions", serde_json::json!(total_versions > 3))
        .set("restore_url_prefix", serde_json::json!(format!("/admin/globals/{}", slug)))
        .set("versions_url", serde_json::json!(format!("/admin/globals/{}/versions", slug)))
        .set("doc_status", serde_json::json!(doc_status))
        .merge(locale_data)
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "globals/edit", &data).into_response()
}

/// POST /admin/globals/{slug} — update a global
pub async fn update_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Form(mut form_data): Form<HashMap<String, String>>,
) -> axum::response::Response {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin"),
    };

    // Check update access
    match check_access_or_forbid(&state, def.access.update.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to update this global").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    // Extract action (publish/save_draft/unpublish) and locale
    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    // Extract locale before it enters hooks/regular data flow
    let form_locale = form_data.remove("_locale");
    let locale_ctx = LocaleContext::from_locale_string(
        form_locale.as_deref(), &state.config.locale,
    );

    // Strip field-level update-denied fields (skip pool.get if no field-level access configured)
    if def.fields.iter().any(|f| f.access.update.is_some()) {
        let user_doc = get_user_doc(&auth_user);
        if let Ok(conn) = state.pool.get() {
            let denied = state.hook_runner.check_field_write_access(&def.fields, user_doc, "update", &conn);
            for name in &denied {
                form_data.remove(name);
            }
        }
    }

    // Convert comma-separated multi-select values to JSON arrays
    transform_select_has_many(&mut form_data, &def.fields);

    // Extract join table data (arrays, blocks, has-many) before sending to service
    let join_data = extract_join_data_from_form(&form_data, &def.fields);

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let form_data_clone = form_data.clone();
    let join_data_clone = join_data.clone();
    let user_doc = get_user_doc(&auth_user).cloned();
    let locale = locale_ctx.as_ref().and_then(|ctx| match &ctx.mode {
        query::LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });
    let action_owned = action.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Handle unpublish: set _status to 'draft' and create a version
        if action_owned == "unpublish" && def_owned.has_versions() {
            let global_table = format!("_global_{}", slug_owned);
            let mut conn = pool.get().map_err(|e| anyhow::anyhow!("DB connection: {}", e))?;
            let tx = conn.transaction().map_err(|e| anyhow::anyhow!("Start transaction: {}", e))?;
            let doc = query::get_global(&tx, &slug_owned, &def_owned, locale_ctx.as_ref())?;
            do_unpublish(&tx, &global_table, "default", &def_owned.fields, def_owned.versions.as_ref(), &doc)?;
            tx.commit().map_err(|e| anyhow::anyhow!("Commit: {}", e))?;
            Ok((doc, HashMap::new()))
        } else {
            crate::service::update_global_document(
                &pool, &runner, &slug_owned, &def_owned,
                form_data, &join_data, locale_ctx.as_ref(), locale,
                user_doc.as_ref(), draft,
            )
        }
    }).await;

    let locale_suffix = form_locale
        .as_ref()
        .filter(|_| state.config.locale.is_enabled())
        .map(|l| format!("?locale={}", l))
        .unwrap_or_default();
    match result {
        Ok(Ok((doc, _req_context))) => {
            state.hook_runner.publish_event(
                &state.event_bus, &def.hooks, def.live.as_ref(),
                crate::core::event::EventTarget::Global,
                crate::core::event::EventOperation::Update,
                slug.clone(), doc.id.clone(), doc.fields.clone(),
                get_event_user(&auth_user),
            );
            htmx_redirect(&format!("/admin/globals/{}{}", slug, locale_suffix))
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let error_map = ve.to_field_map();
                let mut fields = build_field_contexts(&def.fields, &form_data_clone, &error_map, false, false);

                // Enrich relationship/array/blocks fields with options and join data
                let doc_fields: HashMap<String, serde_json::Value> = form_data_clone.iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .chain(join_data_clone.iter().map(|(k, v)| (k.clone(), v.clone())))
                    .collect();
                enrich_field_contexts(&mut fields, &def.fields, &doc_fields, &state, false, false, &error_map, None);

                let form_data_json = serde_json::json!(doc_fields);
                apply_display_conditions(&mut fields, &def.fields, &form_data_json, &state.hook_runner, false);

                let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

                let data = ContextBuilder::new(&state, None)
                    .locale_from_auth(&auth_user)
                    .page(PageType::GlobalEdit, def.display_name())
                    .global_def(&def)
                    .fields(main_fields)
                    .set("sidebar_fields", serde_json::json!(sidebar_fields))
                    .build();
                html_with_toast(&state, "globals/edit", &data, &e.to_string())
            } else {
                tracing::error!("Global update error: {}", e);
                redirect_response(&format!("/admin/globals/{}", slug))
            }
        }
        Err(e) => {
            tracing::error!("Global update task error: {}", e);
            redirect_response(&format!("/admin/globals/{}", slug))
        }
    }
}

/// GET /admin/globals/{slug}/versions — dedicated version history page
pub async fn list_versions_page(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Query(params): Query<PaginationParams>,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return not_found(&state, &format!("Global '{}' not found", slug)).into_response(),
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/globals/{}", slug)).into_response();
    }

    // Check read access
    match check_access_or_forbid(&state, def.access.read.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to view this global").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    let global_table = format!("_global_{}", slug);
    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page
        .unwrap_or(state.config.pagination.default_limit)
        .min(state.config.pagination.max_limit);
    let offset = (page - 1) * per_page;

    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return server_error(&state, "Database error").into_response(),
    };

    let total = query::count_versions(&conn, &global_table, "default").unwrap_or(0);
    let versions: Vec<serde_json::Value> = query::list_versions(&conn, &global_table, "default", Some(per_page), Some(offset))
        .unwrap_or_default()
        .into_iter()
        .map(version_to_json)
        .collect();

    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .page(PageType::GlobalVersions, format!("Version History — {}", def.display_name()))
        .set("page_title", serde_json::json!(format!("Version History — {}", def.display_name())))
        .global_def(&def)
        .set("versions", serde_json::json!(versions))
        .set("restore_url_prefix", serde_json::json!(format!("/admin/globals/{}", slug)))
        .pagination(
            page, per_page, total,
            format!("/admin/globals/{}/versions?page={}", slug, page - 1),
            format!("/admin/globals/{}/versions?page={}", slug, page + 1),
        )
        .breadcrumbs(vec![
            Breadcrumb::link("Dashboard", "/admin"),
            Breadcrumb::link(def.display_name(), format!("/admin/globals/{}", slug)),
            Breadcrumb::current("Version History"),
        ])
        .build();

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "globals/versions", &data).into_response()
}

/// POST /admin/globals/{slug}/versions/{version_id}/restore
pub async fn restore_version(
    State(state): State<AdminState>,
    Path((slug, version_id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin"),
    };

    if !def.has_versions() {
        return redirect_response(&format!("/admin/globals/{}", slug));
    }

    // Check update access
    match check_access_or_forbid(&state, def.access.update.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to update this global").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    let pool = state.pool.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let locale_config = state.config.locale.clone();
    let result = tokio::task::spawn_blocking(move || {
        let global_table = format!("_global_{}", slug_owned);
        let mut conn = pool.get().map_err(|e| anyhow::anyhow!("DB connection: {}", e))?;
        let tx = conn.transaction().map_err(|e| anyhow::anyhow!("Start transaction: {}", e))?;
        let version = query::find_version_by_id(&tx, &global_table, &version_id)?
            .ok_or_else(|| anyhow::anyhow!("Version not found"))?;
        let doc = query::restore_global_version(&tx, &slug_owned, &def_owned, &version.snapshot, "published", &locale_config)?;
        tx.commit().map_err(|e| anyhow::anyhow!("Commit: {}", e))?;
        Ok::<_, anyhow::Error>(doc)
    }).await;

    match result {
        Ok(Ok(_)) => htmx_redirect(&format!("/admin/globals/{}", slug)),
        Ok(Err(e)) => {
            tracing::error!("Restore global version error: {}", e);
            htmx_redirect(&format!("/admin/globals/{}", slug))
        }
        Err(e) => {
            tracing::error!("Restore global version task error: {}", e);
            htmx_redirect(&format!("/admin/globals/{}", slug))
        }
    }
}
