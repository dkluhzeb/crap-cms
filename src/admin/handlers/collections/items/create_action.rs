use axum::{
    Extension,
    extract::{Form, FromRequest, Path, Request, State},
    response::Response,
};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use tokio::task;

use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
        handlers::{
            collections::{
                forms::{
                    extract_join_data_from_form, parse_multipart_form, transform_select_has_many,
                },
                shared::{collect_upload_hidden_fields, render_upload_error},
            },
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
        upload,
        validate::ValidationError,
    },
    db::query::{AccessResult, LocaleContext, LocaleMode},
    hooks::lifecycle::PublishEventInput,
    service,
};

/// POST /admin/collections/{slug} — create a new item
pub async fn create_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    request: Request,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin/collections"),
    };

    // Check create access
    match check_access_or_forbid(&state, def.access.create.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => {
            return forbidden(
                &state,
                "You don't have permission to create items in this collection",
            );
        }
        Err(resp) => return *resp,
        _ => {}
    }

    // Parse form data — multipart for upload collections, regular form for others
    let (mut form_data, file) = if def.is_upload_collection() {
        match parse_multipart_form(request, &state).await {
            Ok(result) => result,
            Err(e) => {
                tracing::error!("Multipart parse error: {}", e);
                return redirect_response(&format!("/admin/collections/{}/create", slug));
            }
        }
    } else {
        let Form(data) = match Form::<HashMap<String, String>>::from_request(request, &state).await
        {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("Form parse error: {}", e);
                return redirect_response(&format!("/admin/collections/{}/create", slug));
            }
        };
        (data, None)
    };

    // Process upload if file present — runs on blocking thread
    let mut queued_conversions = Vec::new();
    let mut upload_guard: Option<upload::CleanupGuard> = None;
    if let Some(f) = file
        && let Some(upload_config) = def.upload.clone()
    {
        let config_dir = state.config_dir.clone();
        let slug_for_upload = slug.clone();
        let global_max = state.config.upload.max_file_size;
        let upload_result = tokio::task::spawn_blocking(move || {
            upload::process_upload(f, &upload_config, &config_dir, &slug_for_upload, global_max)
        })
        .await;

        match upload_result {
            Ok(Ok((processed, guard))) => {
                queued_conversions = processed.queued_conversions.clone();
                upload_guard = Some(guard);

                upload::inject_upload_metadata(&mut form_data, &processed);
            }
            Ok(Err(e)) => {
                tracing::error!("Upload processing error: {}", e);
                return render_upload_error(&state, &def, &form_data, &auth_user, &e.to_string());
            }
            Err(e) => {
                tracing::error!("Upload task error: {}", e);
                return render_upload_error(&state, &def, &form_data, &auth_user, &e.to_string());
            }
        }
    }

    // Strip field-level create-denied fields (fail closed on pool exhaustion)
    if let Err(resp) =
        strip_write_denied_string_fields(&state, &auth_user, &def.fields, "create", &mut form_data)
    {
        return *resp;
    }

    // Extract password before it enters hooks/regular data flow
    let password = if def.is_auth_collection() {
        form_data.remove("password")
    } else {
        None
    };

    // Validate password against policy (create requires a password for auth collections)
    if let Some(ref pw) = password
        && !pw.is_empty()
        && let Err(e) = state.config.auth.password_policy.validate(pw)
    {
        return html_with_toast(&state, "collections/edit_form", &json!({}), &e.to_string());
    }

    // Convert comma-separated multi-select values to JSON arrays
    transform_select_has_many(&mut form_data, &def.fields);

    // Extract join table data (arrays + has-many relationships) from form
    let join_data = extract_join_data_from_form(&form_data, &def.fields);

    // Extract action (publish/save_draft) and locale before they enter hooks
    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let form_locale = form_data.remove("_locale");
    let locale_ctx =
        LocaleContext::from_locale_string(form_locale.as_deref(), &state.config.locale);

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

    let result = task::spawn_blocking(move || {
        service::create_document(
            &pool,
            &runner,
            &slug_owned,
            &def_owned,
            service::WriteInput::builder(form_data, &join_data)
                .password(password.as_deref())
                .locale_ctx(locale_ctx.as_ref())
                .locale(locale)
                .draft(draft)
                .ui_locale(ui_locale)
                .build(),
            user_doc.as_ref(),
        )
    })
    .await;

    match result {
        Ok(Ok((doc, _req_context))) => {
            if let Some(mut g) = upload_guard {
                g.commit();
            }

            // Enqueue deferred image conversions if any
            if !queued_conversions.is_empty()
                && let Ok(conn) = state.pool.get()
                && let Err(e) =
                    upload::enqueue_conversions(&conn, &slug, &doc.id, &queued_conversions)
            {
                tracing::warn!("Failed to enqueue image conversions: {}", e);
            }

            state.hook_runner.publish_event(
                &state.event_bus,
                &def.hooks,
                def.live.as_ref(),
                PublishEventInput::builder(EventTarget::Collection, EventOperation::Create)
                    .collection(slug.clone())
                    .document_id(doc.id.clone())
                    .data(doc.fields.clone())
                    .edited_by(get_event_user(&auth_user))
                    .build(),
            );

            // Auto-send verification email for auth collections with verify_email enabled
            if def.is_auth_collection()
                && def.auth.as_ref().is_some_and(|a| a.verify_email)
                && let Some(user_email) = doc.fields.get("email").and_then(|v| v.as_str())
            {
                service::send_verification_email(
                    state.pool.clone(),
                    state.config.email.clone(),
                    state.email_renderer.clone(),
                    state.config.server.clone(),
                    slug.clone(),
                    doc.id.to_string(),
                    user_email.to_string(),
                );
            }

            htmx_redirect(&format!("/admin/collections/{}", slug))
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
                    build_field_contexts(&def.fields, &form_data_clone, &error_map, true, false);

                enrich_field_contexts(
                    &mut fields,
                    &def.fields,
                    &join_data_clone,
                    &state,
                    &EnrichOptions::builder(&error_map)
                        .filter_hidden(true)
                        .build(),
                );

                let form_json = json!(
                    form_data_clone
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                        .collect::<Map<String, Value>>()
                );

                apply_display_conditions(
                    &mut fields,
                    &def.fields,
                    &form_json,
                    &state.hook_runner,
                    true,
                );

                let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

                let mut data = ContextBuilder::new(&state, None)
                    .locale_from_auth(&auth_user)
                    .filter_nav_by_access(&state, &auth_user)
                    .page(PageType::CollectionCreate, "create_name")
                    .page_title_name(def.singular_name())
                    .collection_def(&def)
                    .fields(main_fields)
                    .set("sidebar_fields", json!(sidebar_fields))
                    .set("editing", json!(false))
                    .set("has_drafts", json!(def.has_drafts()))
                    .build();

                // Preserve upload metadata as hidden inputs so they survive form re-submission
                if def.is_upload_collection() {
                    data["upload_hidden_fields"] =
                        collect_upload_hidden_fields(&def.fields, &form_data_clone);
                }

                html_with_toast(&state, "collections/edit", &data, toast_msg)
            } else {
                tracing::error!("Create error: {}", e);
                redirect_response(&format!("/admin/collections/{}/create", slug))
            }
        }
        Err(e) => {
            tracing::error!("Create task error: {}", e);
            redirect_response(&format!("/admin/collections/{}/create", slug))
        }
    }
}
