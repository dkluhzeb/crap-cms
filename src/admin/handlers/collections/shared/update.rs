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
            forms::FormData,
            shared::{
                forbidden, get_user_doc, htmx_redirect, paths, redirect_response, toast_only_error,
            },
        },
    },
    config::LocaleConfig,
    core::{
        AuthUser, CollectionDefinition, Document, ReqContext, SharedCache, SharedEventTransport,
        SharedInvalidationTransport,
        upload::{UploadedFile, delete_upload_files, enqueue_conversions},
    },
    db::{DbPool, LocaleContext, LocaleMode},
    hooks::HookRunner,
    service::{
        self, ServiceContext, ServiceError,
        auth::{AccountAction, perform_account_action},
    },
};

use super::upload::{UploadParams, UploadResult, process_collection_upload};
use super::{render_form_validation_errors, write_error_toast};

/// Handle post-update success: commit upload, clean old files, enqueue conversions.
fn handle_update_success(state: &AdminState, slug: &str, id: &str, upload: Option<UploadResult>) {
    if let Some(mut ur) = upload {
        ur.guard.commit();

        if let Some(old_fields) = ur.old_doc_fields {
            delete_upload_files(&*state.storage, &old_fields);
        }

        if !ur.queued_conversions.is_empty()
            && let Ok(conn) = state.pool.get()
            && let Err(e) = enqueue_conversions(
                &conn,
                slug,
                id,
                &ur.queued_conversions,
                state.config.jobs.system_image_max_attempts(),
            )
        {
            warn!("Failed to enqueue image conversions: {}", e);
        }
    }
}

/// Whether (and how) to update the auth collection's `_locked` flag.
///
/// Auth collections render a `_locked` checkbox; non-auth collections never
/// touch the lock state. The three states are: skip the update entirely
/// (non-auth), lock the account, or unlock it.
enum LockUpdate {
    Skip,
    Set(bool),
}

/// Prepared update input.
struct UpdateInput {
    form: FormData,
    password: Option<String>,
    lock: LockUpdate,
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
        .invalidation_transport(Some(args.invalidation_bus.clone()))
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
            service::WriteInput::builder(args.input.form)
                .password(args.input.password.as_deref())
                .locale_ctx(args.input.locale_ctx.as_ref())
                .locale(args.locale)
                .draft(args.input.draft)
                .ui_locale(args.ui_locale)
                .build(),
        )
    };

    if result.is_ok()
        && let LockUpdate::Set(should_lock) = args.input.lock
    {
        let conn = args.pool.get().context("DB connection for lock update")?;
        // Gate the lock toggle through `perform_account_action` so the admin
        // surface honors `access.unlock` (`?? update`) against the target user —
        // identical to the gRPC LockAccount/UnlockAccount path. Building a
        // collection context (def + caller `user` + runner) is what lets the
        // access hook run; a `slug_only` context would silently skip the check.
        let ctx = ServiceContext::collection(&args.slug, &args.def)
            .conn(&conn)
            .runner(&args.runner)
            .user(args.user_doc.as_ref())
            .invalidation_transport(Some(args.invalidation_bus))
            .build();

        let action = if should_lock {
            AccountAction::Lock
        } else {
            AccountAction::Unlock
        };
        perform_account_action(&ctx, &args.id, action)?;
    }

    result
}

/// Run the blocking update/unpublish + lock update task.
async fn spawn_update(
    state: &AdminState,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    auth_user: Option<&Extension<AuthUser>>,
    input: UpdateInput,
) -> Result<Result<service::WriteResult, ServiceError>, task::JoinError> {
    let locale = input.locale_ctx.as_ref().and_then(|ctx| match &ctx.mode {
        LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });
    let ui_locale = auth_user.map(|Extension(au)| au.ui_locale.clone());
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
    form_data: HashMap<String, String>,
    file: Option<UploadedFile>,
    auth_user: Option<&Extension<AuthUser>>,
) -> Response {
    let Some(def) = state.registry.get_collection(slug).cloned() else {
        return redirect_response(paths::COLLECTIONS_ROOT).into_response();
    };

    let mut form = FormData::from_raw(form_data, &def.fields);

    let action = form.take_action();
    let draft = action == "save_draft";
    let locale_ctx =
        LocaleContext::from_locale_string(form.take_locale().as_deref(), &state.config.locale)
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
            form.raw_mut(),
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
        form.take("password")
    } else {
        None
    };

    let lock = if def.is_auth_collection() {
        let raw = form.take("_locked");
        let should_lock = matches!(raw.as_deref(), Some("on" | "1"));
        LockUpdate::Set(should_lock)
    } else {
        LockUpdate::Skip
    };

    if let Some(ref pw) = password
        && !pw.is_empty()
        && let Err(e) = state.config.auth.password_policy.validate(pw)
    {
        return toast_only_error(&e.to_string()).into_response();
    }

    let form_for_error = form.clone();

    let result = spawn_update(
        state,
        slug,
        id,
        &def,
        auth_user,
        UpdateInput {
            form,
            password,
            lock,
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
            ServiceError::Validation(ref ve) => {
                render_form_validation_errors(state, &def, Some(id), &form_for_error, ve, auth_user)
                    .into_response()
            }
            // The target document is gone (e.g. deleted in another tab) — a
            // navigation case, not a save error. Redirect back rather than
            // toast a generic "save failed".
            ServiceError::NotFound(_) => {
                redirect_response(&paths::collection_item(slug, id)).into_response()
            }
            other => toast_only_error(&write_error_toast("Update", other, state.pool.kind()))
                .into_response(),
        },
        Err(e) => {
            error!("Update task error: {}", e);
            redirect_response(&paths::collection_item(slug, id))
        }
    }
}
