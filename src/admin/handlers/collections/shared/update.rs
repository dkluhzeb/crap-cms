//! Update handler — processes form submissions for editing collection items.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use axum::{
    Extension,
    response::{IntoResponse, Response},
};
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::{
            forms::FormData,
            shared::{
                get_user_doc, htmx_redirect, parse_request_locale, paths, redirect_response,
                strip_locale_locked_for_publish, toast_only_error,
            },
        },
    },
    core::{
        AuthUser, CollectionDefinition, Document, ReqContext, SharedStorage, upload::UploadedFile,
    },
    db::LocaleContext,
    service::{
        self, AppInfra, ServiceContext, ServiceError,
        auth::{AccountAction, perform_account_action},
        op::{Operation, Unpublish, UnpublishArgs, Update, UpdateArgs},
        upload::{UpdateUploadInput, update_upload},
    },
};

use super::{SubmittedMeta, WriteErrorParams, handle_collection_write_error};

/// Whether (and how) to update the auth collection's `_locked` flag.
///
/// Auth collections render a `_locked` checkbox; non-auth collections never
/// touch the lock state. The three states are: skip the update entirely
/// (non-auth), lock the account, or unlock it.
#[derive(Clone, Copy)]
enum LockUpdate {
    Skip,
    Set(bool),
}

/// Prepared update input.
struct UpdateInput {
    form: FormData,
    /// The multipart file, if the submission carried one. Handed to the upload
    /// service inside the blocking task — never processed out here, where a
    /// dropped handler future would leave the stored bytes behind a document
    /// the blocking task went on to commit.
    file: Option<UploadedFile>,
    password: Option<String>,
    lock: LockUpdate,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
    action: String,
}

/// Owned bundle for the spawn-blocking update body. Process-stable dependencies
/// come from the shared [`AppInfra`]; the rest is per-call.
struct UpdateBlockingInput {
    infra: Arc<AppInfra>,
    slug: String,
    id: String,
    def: CollectionDefinition,
    user_doc: Option<Document>,
    ui_locale: Option<String>,
    max_file_size: u64,
    image_max_attempts: u32,
    input: UpdateInput,
}

/// Everything [`run_write`] needs to perform the document write itself. All
/// fields are required and it is built in exactly one place.
struct WriteArgs<'a> {
    id: &'a str,
    def: &'a CollectionDefinition,
    storage: &'a SharedStorage,
    ui_locale: Option<String>,
    max_file_size: u64,
    image_max_attempts: u32,
    input: UpdateInput,
}

/// The document write: an unpublish action, an upload write, or a plain update.
///
/// A submission that carries a file goes through `service::upload`, the one
/// entry that owns the file lifecycle (store, metadata, the previous file's
/// bytes and its queued conversions) for every surface.
fn run_write(
    ctx: &ServiceContext<'_>,
    args: WriteArgs<'_>,
) -> Result<service::WriteResult, ServiceError> {
    // Route an unpublish action to the unpublish path regardless of versioning:
    // the shared service gate rejects unpublish on a non-versioned collection
    // (an explicit error) rather than silently doing a normal update, matching
    // the Lua surface. It never stores a file — an edit form submitted with
    // both a new file and "unpublish" must not replace the document's file
    // behind a write that does not record it.
    if args.input.action == "unpublish" {
        let doc = Unpublish::run(ctx, UnpublishArgs::new(args.id))?;

        return Ok((doc, ReqContext::new()));
    }

    if let Some(file) = args.input.file {
        let result = update_upload(
            ctx,
            UpdateUploadInput {
                id: args.id,
                storage: args.storage,
                file: Some(file),
                form: args.input.form,
                locale_ctx: args.input.locale_ctx.as_ref(),
                password: args.input.password,
                ui_locale: args.ui_locale,
                draft: args.input.draft,
                upload_max_file_size: args.max_file_size,
                image_max_attempts: args.image_max_attempts,
                form_echoes_locked_fields: true,
            },
        )?;

        return Ok((result.doc, result.req_context));
    }

    let data = strip_locale_locked_for_publish(
        args.input.form.into(),
        &args.def.fields,
        args.input.locale_ctx.as_ref(),
        args.input.draft,
    );

    let op_args = UpdateArgs::builder(args.id, data)
        .password(args.input.password)
        .locale_ctx(args.input.locale_ctx)
        .draft(args.input.draft)
        .build();

    Update::run(ctx, op_args)
}

/// Apply the auth collection's `_locked` toggle after a successful write.
///
/// Gated through `perform_account_action` so the admin surface honors
/// `access.unlock` (`?? update`) against the target user — identical to the
/// gRPC `LockAccount`/`UnlockAccount` path. Building a collection context
/// (def + caller `user` + runner) is what lets the access hook run; a
/// `slug_only` context would silently skip the check.
fn apply_lock(
    infra: &AppInfra,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    user_doc: Option<&Document>,
    should_lock: bool,
) -> Result<(), ServiceError> {
    let conn = infra.pool.get().context("DB connection for lock update")?;

    let ctx = ServiceContext::collection(slug, def)
        .conn(&conn)
        .runner(&infra.hook_runner)
        .user(user_doc)
        .invalidation_transport(Some(infra.invalidation_transport.clone()))
        .build();

    let action = if should_lock {
        AccountAction::Lock
    } else {
        AccountAction::Unlock
    };

    perform_account_action(&ctx, id, action)?;

    Ok(())
}

/// Synchronous body of [`spawn_update`]. Builds the service context, runs the
/// write, and applies the optional account-lock toggle for auth collections.
fn update_document_blocking(
    args: UpdateBlockingInput,
) -> Result<service::WriteResult, ServiceError> {
    let ctx = ServiceContext::collection(&args.slug, &args.def)
        .infra(&args.infra)
        .user(args.user_doc.as_ref())
        .ui_locale(args.ui_locale.clone())
        .build();

    let lock = args.input.lock;

    let result = run_write(
        &ctx,
        WriteArgs {
            id: &args.id,
            def: &args.def,
            storage: &args.infra.storage,
            ui_locale: args.ui_locale.clone(),
            max_file_size: args.max_file_size,
            image_max_attempts: args.image_max_attempts,
            input: args.input,
        },
    );

    if result.is_ok()
        && let LockUpdate::Set(should_lock) = lock
    {
        apply_lock(
            &args.infra,
            &args.slug,
            &args.id,
            &args.def,
            args.user_doc.as_ref(),
            should_lock,
        )?;
    }

    result
}

/// Run the blocking write + lock update task.
async fn spawn_update(
    state: &AdminState,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    auth_user: Option<&Extension<AuthUser>>,
    input: UpdateInput,
) -> Result<Result<service::WriteResult, ServiceError>, task::JoinError> {
    let ui_locale = auth_user.map(|Extension(au)| au.ui_locale.clone());
    // The unpublish branch reads the row via `find_by_id_raw`, which needs
    // a `LocaleContext` to emit `title__en`/`title__de` for localized
    // fields when locales are enabled. Threading the config through
    // `ServiceContext` lets the service build a default `All` context.
    let args = UpdateBlockingInput {
        infra: state.infra.clone(),
        slug: slug.to_string(),
        id: id.to_string(),
        def: def.clone(),
        user_doc: get_user_doc(auth_user).cloned(),
        ui_locale,
        max_file_size: state.config.upload.max_file_size,
        image_max_attempts: state.config.jobs.system_image_max_attempts(),
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
    let Some(def) = state.infra.registry.get_collection(slug).cloned() else {
        return redirect_response(paths::COLLECTIONS_ROOT).into_response();
    };

    let mut form = FormData::from_raw(form_data, &def.fields);

    let action = form.take_action();
    let draft = action == "save_draft";

    // Kept past the write: an error re-render has to put `_locale` back into
    // the form, or the corrected save lands in the default locale.
    let submitted_locale = form.take_locale();
    let locale_ctx = match parse_request_locale(submitted_locale.as_deref(), &state.config.locale) {
        Ok(ctx) => ctx,
        Err(msg) => return toast_only_error(&msg),
    };

    // Field and collection write access are checked inside the service write.
    let password = form.take_password(&def);

    // Likewise kept: an error re-render that drops the lock box would post no
    // `_locked` on the corrected save, which reads as an explicit unlock.
    let submitted_lock = def.is_auth_collection().then(|| {
        let raw = form.take("_locked");

        matches!(raw.as_deref(), Some("on" | "1"))
    });

    let lock = match submitted_lock {
        Some(should_lock) => LockUpdate::Set(should_lock),
        None => LockUpdate::Skip,
    };

    if let Some(ref pw) = password
        && !pw.is_empty()
        && let Err(e) = state.config.auth.password_policy.validate(pw)
    {
        return toast_only_error(&e.to_string()).into_response();
    }

    let form_for_error = form.clone();

    // A file only reaches the write when the collection accepts one; on any
    // other collection it is ignored exactly as before.
    let file = file.filter(|_| def.is_upload_collection());

    let result = spawn_update(
        state,
        slug,
        id,
        &def,
        auth_user,
        UpdateInput {
            form,
            file,
            password,
            lock,
            locale_ctx,
            draft,
            action,
        },
    )
    .await;

    match result {
        Ok(Ok(_)) => htmx_redirect(&paths::collection_item(slug, id)),
        Ok(Err(e)) => {
            handle_collection_write_error(WriteErrorParams {
                state,
                def: &def,
                form: &form_for_error,
                err: e,
                doc_id: Some(id),
                auth_user,
                meta: SubmittedMeta::new(submitted_locale.as_deref(), submitted_lock),
            })
            .await
        }
        Err(e) => {
            error!("Update task error: {}", e);
            redirect_response(&paths::collection_item(slug, id))
        }
    }
}
