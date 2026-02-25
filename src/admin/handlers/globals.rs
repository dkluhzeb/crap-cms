//! Global edit and update handlers.

use axum::{
    extract::{Form, Path, Query, State},
    response::IntoResponse,
    Extension,
};
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::core::auth::{AuthUser, Claims};
use crate::core::validate::ValidationError;
use crate::db::{ops, query};
use crate::db::query::{AccessResult, LocaleContext};
use crate::hooks::lifecycle::HookEvent;

use super::shared::{
    LocaleParams,
    user_json, get_user_doc,
    check_access_or_forbid, build_locale_template_data,
    is_non_default_locale,
    build_field_contexts, enrich_field_contexts,
    forbidden, redirect_response, html_with_toast,
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
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(e) => return server_error(&state, &format!("Registry lock poisoned: {}", e)).into_response(),
        };
        match reg.get_global(&slug) {
            Some(d) => d.clone(),
            None => return not_found(&state, &format!("Global '{}' not found", slug)).into_response(),
        }
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

    // Strip field-level read-denied fields
    let mut doc_fields = document.fields.clone();
    {
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
    enrich_field_contexts(&mut fields, &def.fields, &doc_fields, &state, false, non_default_locale);

    let mut data = serde_json::json!({
        "page_title": def.display_name(),
        "collections": state.sidebar_collections(),
        "globals": state.sidebar_globals(),
        "global": {
            "slug": def.slug,
            "display_name": def.display_name(),
        },
        "fields": fields,
        "user": user_json(&claims),
        "breadcrumbs": [
            { "label": "Dashboard", "url": "/admin" },
            { "label": def.display_name() },
        ],
    });

    // Merge locale data into template context
    if let Some(obj) = locale_data.as_object() {
        for (k, v) in obj {
            data[k] = v.clone();
        }
    }

    render_or_error(&state, "globals/edit", &data).into_response()
}

/// POST /admin/globals/{slug} — update a global
pub async fn update_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Form(mut form_data): Form<HashMap<String, String>>,
) -> axum::response::Response {
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(_) => return redirect_response("/admin"),
        };
        reg.get_global(&slug).cloned()
    };
    let def = match def {
        Some(d) => d,
        None => return redirect_response("/admin"),
    };

    // Check update access
    match check_access_or_forbid(&state, def.access.update.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => return forbidden(&state, "You don't have permission to update this global").into_response(),
        Err(resp) => return resp,
        _ => {}
    }

    // Extract locale before it enters hooks/regular data flow
    let form_locale = form_data.remove("_locale");
    let locale_ctx = LocaleContext::from_locale_string(
        form_locale.as_deref(), &state.config.locale,
    );

    // Strip field-level update-denied fields
    {
        let user_doc = get_user_doc(&auth_user);
        if let Ok(conn) = state.pool.get() {
            let denied = state.hook_runner.check_field_write_access(&def.fields, user_doc, "update", &conn);
            for name in &denied {
                form_data.remove(name);
            }
        }
    }

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let form_data_clone = form_data.clone();
    let user_doc = get_user_doc(&auth_user).cloned();
    let locale = locale_ctx.as_ref().and_then(|ctx| match &ctx.mode {
        query::LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });
    let result = tokio::task::spawn_blocking(move || {
        crate::service::update_global_document(
            &pool, &runner, &slug_owned, &def_owned,
            form_data, locale_ctx.as_ref(), locale,
            user_doc.as_ref(),
        )
    }).await;

    let locale_suffix = form_locale
        .as_ref()
        .filter(|_| state.config.locale.is_enabled())
        .map(|l| format!("?locale={}", l))
        .unwrap_or_default();
    match result {
        Ok(Ok(doc)) => {
            state.hook_runner.fire_after_event(
                &def.hooks, &def.fields, HookEvent::AfterChange,
                slug.clone(), "update".to_string(), doc.fields.clone(),
            );
            state.hook_runner.publish_event(
                &state.event_bus, &def.hooks, def.live.as_ref(),
                crate::core::event::EventTarget::Global,
                crate::core::event::EventOperation::Update,
                slug.clone(), doc.id.clone(), doc.fields.clone(),
            );
            redirect_response(&format!("/admin/globals/{}{}", slug, locale_suffix))
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let error_map = ve.to_field_map();
                let fields = build_field_contexts(&def.fields, &form_data_clone, &error_map, false, false);
                let data = serde_json::json!({
                    "page_title": def.display_name(),
                    "collections": state.sidebar_collections(),
                    "globals": state.sidebar_globals(),
                    "global": {
                        "slug": def.slug,
                        "display_name": def.display_name(),
                    },
                    "fields": fields,
                });
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
