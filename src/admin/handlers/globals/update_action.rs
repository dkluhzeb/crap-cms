use anyhow::anyhow;
use axum::{
    Extension,
    extract::{Form, Path, State},
    response::Response,
};
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
        handlers::{
            collections::forms::{extract_join_data_from_form, transform_select_has_many},
            shared::{
                EnrichOptions, apply_display_conditions, build_field_contexts,
                check_access_or_forbid, enrich_field_contexts, forbidden, get_event_user,
                get_user_doc, html_with_toast, htmx_redirect, redirect_response,
                split_sidebar_fields, strip_write_denied_string_fields,
                translate_validation_errors,
            },
        },
    },
    core::{
        auth::AuthUser,
        event::{EventOperation, EventTarget},
        validate::ValidationError,
    },
    db::query::{self, AccessResult, LocaleContext, LocaleMode},
    hooks::lifecycle::PublishEventInput,
    service,
};

/// POST /admin/globals/{slug} — update a global
pub async fn update_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Form(mut form_data): Form<HashMap<String, String>>,
) -> Response {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin"),
    };

    // Check update access
    match check_access_or_forbid(&state, def.access.update.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => {
            return forbidden(&state, "You don't have permission to update this global");
        }
        Err(resp) => return *resp,
        _ => {}
    }

    // Extract action (publish/save_draft/unpublish) and locale
    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let form_locale = form_data.remove("_locale");
    let locale_ctx =
        LocaleContext::from_locale_string(form_locale.as_deref(), &state.config.locale);

    // Strip field-level update-denied fields (fail closed on pool exhaustion)
    if let Err(resp) =
        strip_write_denied_string_fields(&state, &auth_user, &def.fields, "update", &mut form_data)
    {
        return *resp;
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
            let mut conn = pool.get().map_err(|e| anyhow!("DB connection: {}", e))?;
            let tx = conn
                .transaction()
                .map_err(|e| anyhow!("Start transaction: {}", e))?;
            let doc = query::get_global(&tx, &slug_owned, &def_owned, locale_ctx.as_ref())?;

            service::unpublish_with_snapshot(
                &tx,
                &global_table,
                "default",
                &def_owned.fields,
                def_owned.versions.as_ref(),
                &doc,
            )?;

            tx.commit().map_err(|e| anyhow!("Commit: {}", e))?;

            Ok((doc, HashMap::new()))
        } else {
            service::update_global_document(
                &pool,
                &runner,
                &slug_owned,
                &def_owned,
                service::WriteInput::builder(form_data, &join_data)
                    .locale_ctx(locale_ctx.as_ref())
                    .locale(locale)
                    .draft(draft)
                    .ui_locale(ui_locale)
                    .build(),
                user_doc.as_ref(),
            )
        }
    })
    .await;

    match result {
        Ok(Ok((doc, _req_context))) => {
            state.hook_runner.publish_event(
                &state.event_bus,
                &def.hooks,
                def.live.as_ref(),
                PublishEventInput::builder(EventTarget::Global, EventOperation::Update)
                    .collection(slug.clone())
                    .document_id(doc.id.clone())
                    .data(doc.fields.clone())
                    .edited_by(get_event_user(&auth_user))
                    .build(),
            );
            htmx_redirect(&format!("/admin/globals/{}", slug))
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let locale = auth_user
                    .as_ref()
                    .map(|Extension(au)| au.ui_locale.as_str())
                    .unwrap_or("en");
                let error_map = translate_validation_errors(ve, &state.translations, locale);
                let toast_msg = state.translations.get(locale, "validation.error_summary");
                let mut fields =
                    build_field_contexts(&def.fields, &form_data_clone, &error_map, false, false);

                // Enrich relationship/array/blocks fields with options and join data
                let doc_fields: HashMap<String, Value> = form_data_clone
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .chain(join_data_clone.iter().map(|(k, v)| (k.clone(), v.clone())))
                    .collect();

                enrich_field_contexts(
                    &mut fields,
                    &def.fields,
                    &doc_fields,
                    &state,
                    &EnrichOptions::builder(&error_map).build(),
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

                let data = ContextBuilder::new(&state, None)
                    .locale_from_auth(&auth_user)
                    .page(PageType::GlobalEdit, def.display_name())
                    .global_def(&def)
                    .fields(main_fields)
                    .set("sidebar_fields", json!(sidebar_fields))
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
