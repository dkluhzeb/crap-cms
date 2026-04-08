//! Update handler — processes form submissions for editing collection items.

use std::collections::HashMap;

use anyhow::Context;
use axum::{
    Extension,
    response::{IntoResponse, Response},
};
use serde_json::Value;
use tokio::task;
use tracing::{error, warn};

use crate::{
    admin::{
        AdminState,
        handlers::{
            forms::{extract_join_data_from_form, transform_select_has_many},
            shared::{
                forbidden, get_event_user, get_user_doc, htmx_redirect, redirect_response,
                toast_only_error,
            },
        },
    },
    core::{
        Document,
        auth::AuthUser,
        collection::CollectionDefinition,
        event::{EventOperation, EventTarget},
        upload::{UploadedFile, delete_upload_files, enqueue_conversions},
    },
    db::query::{self, LocaleContext, LocaleMode},
    hooks::lifecycle::PublishEventInput,
    service::{self, ServiceError},
};

use super::render_form_validation_errors;
use super::upload::{UploadParams, UploadResult, process_collection_upload};

/// Handle post-update success: commit upload, clean old files, enqueue conversions, publish event.
fn handle_update_success(
    state: &AdminState,
    def: &CollectionDefinition,
    slug: &str,
    id: &str,
    doc: &Document,
    upload: Option<UploadResult>,
    auth_user: &Option<Extension<AuthUser>>,
) {
    if let Some(mut ur) = upload {
        ur.guard.commit();

        if let Some(old_fields) = ur.old_doc_fields {
            delete_upload_files(&*state.storage, &old_fields);
        }

        if !ur.queued_conversions.is_empty()
            && let Ok(conn) = state.pool.get()
            && let Err(e) = enqueue_conversions(&conn, slug, id, &ur.queued_conversions)
        {
            warn!("Failed to enqueue image conversions: {}", e);
        }
    }

    state.hook_runner.publish_event(
        &state.event_bus,
        &def.hooks,
        def.live.as_ref(),
        PublishEventInput::builder(EventTarget::Collection, EventOperation::Update)
            .collection(slug.to_string())
            .document_id(id.to_string())
            .data(doc.fields.clone())
            .edited_by(get_event_user(auth_user))
            .build(),
    );
}

/// Prepared update input.
struct UpdateInput {
    form_data: HashMap<String, String>,
    join_data: HashMap<String, Value>,
    password: Option<String>,
    locked_value: Option<Option<String>>,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
    action: String,
}

/// Run the blocking update/unpublish + lock update task.
async fn spawn_update(
    state: &AdminState,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    auth_user: &Option<Extension<AuthUser>>,
    input: UpdateInput,
) -> Result<Result<service::WriteResult, ServiceError>, task::JoinError> {
    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.to_string();
    let id_owned = id.to_string();
    let def_owned = def.clone();
    let user_doc = get_user_doc(auth_user).cloned();
    let locale = input.locale_ctx.as_ref().and_then(|ctx| match &ctx.mode {
        LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });
    let ui_locale = auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone());

    task::spawn_blocking(move || {
        let result = if input.action == "unpublish" && def_owned.has_versions() {
            let doc = service::unpublish_document(
                &pool,
                &runner,
                &slug_owned,
                &id_owned,
                &def_owned,
                user_doc.as_ref(),
            )?;

            Ok((doc, HashMap::new()))
        } else {
            service::update_document(
                &pool,
                &runner,
                &slug_owned,
                &id_owned,
                &def_owned,
                service::WriteInput::builder(input.form_data, &input.join_data)
                    .password(input.password.as_deref())
                    .locale_ctx(input.locale_ctx.as_ref())
                    .locale(locale)
                    .draft(input.draft)
                    .ui_locale(ui_locale)
                    .build(),
                user_doc.as_ref(),
            )
        };

        if result.is_ok()
            && let Some(locked_field) = input.locked_value
        {
            let should_lock =
                locked_field.as_deref() == Some("on") || locked_field.as_deref() == Some("1");
            let conn = pool.get().context("DB connection for lock update")?;

            if should_lock {
                query::auth::lock_user(&conn, &slug_owned, &id_owned)?;
            } else {
                query::auth::unlock_user(&conn, &slug_owned, &id_owned)?;
            }
        }

        result
    })
    .await
}

/// Process a form update for a collection item (called from `update_action.rs`).
pub(in crate::admin::handlers::collections) async fn do_update(
    state: &AdminState,
    slug: &str,
    id: &str,
    mut form_data: HashMap<String, String>,
    file: Option<UploadedFile>,
    auth_user: &Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_collection(slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin/collections").into_response(),
    };

    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";
    let form_locale = form_data.remove("_locale");
    let locale_ctx =
        LocaleContext::from_locale_string(form_locale.as_deref(), &state.config.locale);

    let mut upload_result = None;

    if let Some(f) = file
        && def.upload.is_some()
    {
        match process_collection_upload(
            &UploadParams {
                state,
                def: &def,
                slug,
                doc_id: Some(id),
                locale_ctx: locale_ctx.as_ref(),
                auth_user,
            },
            &mut form_data,
            f,
        )
        .await
        {
            Ok(ur) => upload_result = Some(ur),
            Err(resp) => return resp.into_response(),
        }
    }

    // Field write access is now checked inside service::update_document_core.

    let password = if def.is_auth_collection() {
        form_data.remove("password")
    } else {
        None
    };

    let locked_value = if def.is_auth_collection() {
        Some(form_data.remove("_locked"))
    } else {
        None
    };

    if let Some(ref pw) = password
        && !pw.is_empty()
        && let Err(e) = state.config.auth.password_policy.validate(pw)
    {
        return toast_only_error(&e.to_string()).into_response();
    }

    transform_select_has_many(&mut form_data, &def.fields);
    let join_data = extract_join_data_from_form(&form_data, &def.fields);
    let form_data_clone = form_data.clone();
    let join_data_clone = join_data.clone();

    let result = spawn_update(
        state,
        slug,
        id,
        &def,
        auth_user,
        UpdateInput {
            form_data,
            join_data,
            password,
            locked_value,
            locale_ctx,
            draft,
            action,
        },
    )
    .await;

    match result {
        Ok(Ok((doc, _req_context))) => {
            handle_update_success(state, &def, slug, id, &doc, upload_result, auth_user);

            htmx_redirect(&format!("/admin/collections/{}/{}", slug, id))
        }
        Ok(Err(e)) => match e {
            ServiceError::AccessDenied(_) => {
                forbidden(state, "You don't have permission to update this item").into_response()
            }
            ServiceError::Validation(ref ve) => render_form_validation_errors(
                state,
                &def,
                Some(id),
                &form_data_clone,
                &join_data_clone,
                ve,
                auth_user,
            )
            .into_response(),
            other => {
                error!("Update error: {}", other);
                redirect_response(&format!("/admin/collections/{}/{}", slug, id))
            }
        },
        Err(e) => {
            error!("Update task error: {}", e);
            redirect_response(&format!("/admin/collections/{}/{}", slug, id))
        }
    }
}
