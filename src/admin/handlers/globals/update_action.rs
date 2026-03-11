use axum::{
    extract::{Form, Path, State},
    response::IntoResponse,
    Extension,
};
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::admin::context::{ContextBuilder, PageType};
use crate::admin::handlers::collections::forms::{extract_join_data_from_form, transform_select_has_many};
use crate::core::auth::AuthUser;
use crate::core::validate::ValidationError;
use crate::db::query::{self, AccessResult, LocaleContext, LocaleMode};
use crate::core::event::{EventTarget, EventOperation};
use crate::service;

use crate::admin::handlers::shared::{
    get_user_doc, get_event_user,
    check_access_or_forbid, 
    build_field_contexts, enrich_field_contexts,
    apply_display_conditions, split_sidebar_fields,
    translate_validation_errors,
    do_unpublish,
    htmx_redirect, html_with_toast,
    redirect_response, server_error, forbidden,
};

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

    let form_locale = form_data.remove("_locale");
    let locale_ctx = LocaleContext::from_locale_string(
        form_locale.as_deref(), &state.config.locale,
    );

    // Strip field-level update-denied fields (fail closed on pool exhaustion)
    if def.fields.iter().any(|f| f.access.update.is_some()) {
        let user_doc = get_user_doc(&auth_user);
        let mut conn = match state.pool.get() {
            Ok(c) => c,
            Err(e) => { tracing::error!("Field access check pool error: {}", e); return server_error(&state, "Database error").into_response(); }
        };
        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(e) => { tracing::error!("Field access check tx error: {}", e); return server_error(&state, "Database error").into_response(); }
        };
        let denied = state.hook_runner.check_field_write_access(&def.fields, user_doc, "update", &tx);
        // Read-only access check — commit result is irrelevant, rollback on drop is safe
        let _ = tx.commit();
        for name in &denied {
            form_data.remove(name);
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
        LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });
    let ui_locale = auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone());
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
            service::update_global_document(
                &pool, &runner, &slug_owned, &def_owned,
                service::WriteInput {
                    data: form_data, join_data: &join_data, password: None,
                    locale_ctx: locale_ctx.as_ref(), locale, draft, ui_locale,
                },
                user_doc.as_ref(),
            )
        }
    }).await;

    match result {
        Ok(Ok((doc, _req_context))) => {
            state.hook_runner.publish_event(
                &state.event_bus, &def.hooks, def.live.as_ref(),
                EventTarget::Global,
                EventOperation::Update,
                slug.clone(), doc.id.clone(), doc.fields.clone(),
                get_event_user(&auth_user),
            );
            htmx_redirect(&format!("/admin/globals/{}", slug))
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let locale = auth_user.as_ref()
                    .map(|Extension(au)| au.ui_locale.as_str())
                    .unwrap_or("en");
                let error_map = translate_validation_errors(ve, &state.translations, locale);
                let toast_msg = state.translations.get(locale, "validation.error_summary");
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
                html_with_toast(&state, "globals/edit", &data, toast_msg)
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
