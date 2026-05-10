//! Update handler — processes form submissions for editing collection items.

use std::collections::HashMap;

use anyhow::Context;
use axum::{
    Extension,
    response::{IntoResponse, Response},
};
use tokio::task;
use tracing::{error, warn};

use crate::{
    admin::{
        AdminState,
        handlers::{
            forms::{extract_join_data_from_form, transform_select_has_many},
            shared::{
                forbidden, get_user_doc, htmx_redirect, paths, redirect_response, toast_only_error,
            },
        },
    },
    config::LocaleConfig,
    core::{
        AuthUser, CollectionDefinition, Document, DocumentFields, ReqContext, SharedCache,
        SharedEventTransport, SharedInvalidationTransport,
        upload::{UploadedFile, delete_upload_files, enqueue_conversions},
    },
    db::{DbPool, LocaleContext, LocaleMode},
    hooks::HookRunner,
    service::{
        self, ServiceContext, ServiceError,
        auth::{lock_user, unlock_user},
    },
};

use super::render_form_validation_errors;
use super::upload::{UploadParams, UploadResult, process_collection_upload};

/// Handle post-update success: commit upload, clean old files, enqueue conversions.
fn handle_update_success(state: &AdminState, slug: &str, id: &str, upload: Option<UploadResult>) {
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
}

/// Prepared update input.
struct UpdateInput {
    form_data: HashMap<String, String>,
    join_data: DocumentFields,
    password: Option<String>,
    locked_value: Option<Option<String>>,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
    action: String,
}

/// Owned bundle for the spawn-blocking update body.
struct UpdateBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    invalidation_bus: SharedInvalidationTransport,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
    slug: String,
    id: String,
    def: CollectionDefinition,
    user_doc: Option<Document>,
    locale: Option<String>,
    ui_locale: Option<String>,
    locale_config: LocaleConfig,
    input: UpdateInput,
}

/// Synchronous body of [`spawn_update`]. Builds the service context, runs
/// either `unpublish_document` (for `action == "unpublish"` on versioned
/// collections) or `update_document`, and applies the optional account-lock
/// toggle for auth collections.
fn update_document_blocking(
    args: UpdateBlockingInput,
) -> Result<service::WriteResult, ServiceError> {
    let ctx = service::ServiceContext::collection(&args.slug, &args.def)
        .pool(&args.pool)
        .runner(&args.runner)
        .user(args.user_doc.as_ref())
        .event_transport(args.event_transport)
        .cache(args.cache)
        .locale_config(Some(&args.locale_config))
        .build();

    let result = if args.input.action == "unpublish" && args.def.has_versions() {
        let doc = service::unpublish_document(&ctx, &args.id)?;

        Ok((doc, ReqContext::new()))
    } else {
        service::update_document(
            &ctx,
            &args.id,
            service::WriteInput::builder({
                let mut __m = service::values_from_strings(args.input.form_data);
                for (k, v) in args.input.join_data.iter() {
                    __m.insert(k.clone(), v.clone());
                }
                __m
            })
            .password(args.input.password.as_deref())
            .locale_ctx(args.input.locale_ctx.as_ref())
            .locale(args.locale)
            .draft(args.input.draft)
            .ui_locale(args.ui_locale)
            .build(),
        )
    };

    if result.is_ok()
        && let Some(locked_field) = args.input.locked_value
    {
        let should_lock =
            locked_field.as_deref() == Some("on") || locked_field.as_deref() == Some("1");
        let conn = args.pool.get().context("DB connection for lock update")?;
        let ctx = ServiceContext::slug_only(&args.slug)
            .conn(&conn)
            .invalidation_transport(Some(args.invalidation_bus))
            .build();

        if should_lock {
            lock_user(&ctx, &args.id)?;
        } else {
            unlock_user(&ctx, &args.id)?;
        }
    }

    result
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
    let locale = input.locale_ctx.as_ref().and_then(|ctx| match &ctx.mode {
        LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });
    let ui_locale = auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone());
    // The unpublish branch reads the row via `find_by_id_raw`, which needs
    // a `LocaleContext` to emit `title__en`/`title__de` for localized
    // fields when locales are enabled. Threading the config through
    // `ServiceContext` lets the service build a default `All` context.
    let args = UpdateBlockingInput {
        pool: state.pool.clone(),
        runner: state.hook_runner.clone(),
        invalidation_bus: state.invalidation_transport.clone(),
        event_transport: state.event_transport.clone(),
        cache: state.cache.clone(),
        slug: slug.to_string(),
        id: id.to_string(),
        def: def.clone(),
        user_doc: get_user_doc(auth_user).cloned(),
        locale,
        ui_locale,
        locale_config: state.config.locale.clone(),
        input,
    };

    task::spawn_blocking(move || update_document_blocking(args)).await
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
    let Some(def) = state.registry.get_collection(slug).cloned() else {
        return redirect_response(paths::COLLECTIONS_ROOT).into_response();
    };

    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";
    let form_locale = form_data.remove("_locale");
    let locale_ctx =
        LocaleContext::from_locale_string(form_locale.as_deref(), &state.config.locale)
            .unwrap_or(None);

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

    // Field write access is now checked inside service::update_document_in_conn.

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
        Ok(Ok(_)) => {
            handle_update_success(state, slug, id, upload_result);

            htmx_redirect(&paths::collection_item(slug, id))
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
                redirect_response(&paths::collection_item(slug, id))
            }
        },
        Err(e) => {
            error!("Update task error: {}", e);
            redirect_response(&paths::collection_item(slug, id))
        }
    }
}
