//! Update handler — processes form submissions for editing collection items.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use axum::{
    Extension,
    response::{IntoResponse, Response},
};
use tokio::task::JoinError;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::{
            forms::FormData,
            shared::{
                HxNav, collection_form_fields, get_user_doc, htmx_redirect, parse_request_locale,
                paths, redirect_response, strip_locale_locked_form_fields, toast_only_error,
            },
        },
    },
    core::{
        AuthUser, CollectionDefinition, Document, ReqContext, SharedStorage,
        spawn_request_blocking, upload::UploadedFile,
    },
    db::{BoxedConnection, LocaleContext},
    service::{
        self, AppInfra, ServiceContext, ServiceError,
        auth::{AccountAction, check_account_action_access, is_locked, perform_account_action},
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

/// One edit-form submission, as the route handler received it. All fields are
/// required and it is built in exactly one place.
pub(in crate::admin::handlers::collections) struct UpdateRequest<'a> {
    pub state: &'a AdminState,
    pub slug: &'a str,
    pub id: &'a str,
    pub form_data: HashMap<String, String>,
    pub file: Option<UploadedFile>,
    pub auth_user: Option<&'a Extension<AuthUser>>,
    /// How the submit was issued — decides whether an error re-render answers
    /// with the `#main` fragment or a full document.
    pub hx: HxNav,
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
    /// The revision the edit form was loaded at (`_revision`).
    expected_revision: Option<i64>,
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
        let unpublish = UnpublishArgs::builder(args.id)
            .expected_revision(args.input.expected_revision)
            .build();
        let doc = Unpublish::run(ctx, unpublish)?;

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
                draft: args.input.draft,
                upload_max_file_size: args.max_file_size,
                image_max_attempts: args.image_max_attempts,
                form_echoes_locked_fields: true,
                expected_revision: args.input.expected_revision,
            },
        )?;

        return Ok((result.doc, result.req_context));
    }

    let data = strip_locale_locked_form_fields(
        args.input.form.into(),
        &args.def.fields,
        args.input.locale_ctx.as_ref(),
    );

    let op_args = UpdateArgs::builder(args.id, data)
        .password(args.input.password)
        .locale_ctx(args.input.locale_ctx)
        .draft(args.input.draft)
        .expected_revision(args.input.expected_revision)
        .build();

    Update::run(ctx, op_args)
}

/// The user an account action targets and the context it runs in — every
/// field but the write input, so the input can be handed to the document
/// write while the action waits for it to land.
struct LockTarget<'a> {
    slug: &'a str,
    def: &'a CollectionDefinition,
    id: &'a str,
    user: Option<&'a Document>,
    infra: &'a AppInfra,
}

impl<'a> LockTarget<'a> {
    /// A collection context (def + caller `user` + runner) for the account
    /// action, which is what lets its access hook run; a `slug_only` context
    /// would silently skip the check.
    fn context(&self, conn: &'a BoxedConnection) -> ServiceContext<'a> {
        ServiceContext::collection(self.slug, self.def)
            .conn(conn)
            .runner(&self.infra.hook_runner)
            .user(self.user)
            .invalidation_transport(Some(self.infra.invalidation_transport.clone()))
            .build()
    }

    /// The account action the `_locked` box asks for, or `None` when the box
    /// matches the stored lock state.
    ///
    /// Diffing keeps a save that leaves the box alone from running the
    /// account action at all: an untouched lock needs no `access.unlock`
    /// (which may be narrower than `update`), and re-saving an already locked
    /// user must not bump that user's `_session_version` again.
    fn action(&self, lock: LockUpdate) -> Result<Option<AccountAction>, ServiceError> {
        let LockUpdate::Set(should_lock) = lock else {
            return Ok(None);
        };

        let conn = self
            .infra
            .pool
            .get()
            .context("DB connection for lock state")?;

        if is_locked(&self.context(&conn), self.id)? == should_lock {
            return Ok(None);
        }

        Ok(Some(if should_lock {
            AccountAction::Lock
        } else {
            AccountAction::Unlock
        }))
    }

    /// The access half of the action, before the document write — so a
    /// denied toggle answers 403 with nothing persisted.
    fn check(&self, action: AccountAction) -> Result<(), ServiceError> {
        let conn = self
            .infra
            .pool
            .get()
            .context("DB connection for lock access")?;

        check_account_action_access(&self.context(&conn), self.id, action)
    }

    /// Run the action after the document write landed, through
    /// `perform_account_action` so the admin surface honors `access.unlock`
    /// (`?? update`) against the target user — identical to the gRPC
    /// `LockAccount`/`UnlockAccount` path.
    fn apply(&self, action: AccountAction) -> Result<(), ServiceError> {
        let conn = self
            .infra
            .pool
            .write()
            .context("DB connection for lock update")?;

        perform_account_action(&self.context(&conn), self.id, action)
    }
}

/// Synchronous body of [`spawn_update`]. Checks a changed account-lock box's
/// access before the document write — so a denied toggle answers 403 with
/// nothing persisted — and applies the toggle after the write landed, so a
/// failed save never locks or unlocks an account on its own.
fn update_document_blocking(
    args: UpdateBlockingInput,
) -> Result<service::WriteResult, ServiceError> {
    // Field-level borrows, so the write input below can move out of `args`.
    let target = LockTarget {
        slug: &args.slug,
        def: &args.def,
        id: &args.id,
        user: args.user_doc.as_ref(),
        infra: &args.infra,
    };
    let action = target.action(args.input.lock)?;

    if let Some(action) = action {
        target.check(action)?;
    }

    let ctx = ServiceContext::collection(&args.slug, &args.def)
        .infra(&args.infra)
        .user(args.user_doc.as_ref())
        .ui_locale(args.ui_locale)
        .build();

    let result = run_write(
        &ctx,
        WriteArgs {
            id: &args.id,
            def: &args.def,
            storage: &args.infra.storage,
            max_file_size: args.max_file_size,
            image_max_attempts: args.image_max_attempts,
            input: args.input,
        },
    )?;

    if let Some(action) = action {
        target.apply(action)?;
    }

    Ok(result)
}

/// Run the blocking write + lock update task.
async fn spawn_update(
    state: &AdminState,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    auth_user: Option<&Extension<AuthUser>>,
    input: UpdateInput,
) -> Result<Result<service::WriteResult, ServiceError>, JoinError> {
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

    spawn_request_blocking(move || update_document_blocking(args)).await
}

/// Process a form update for a collection item (called from `update_action.rs`).
pub(in crate::admin::handlers::collections) async fn do_update(req: UpdateRequest<'_>) -> Response {
    let UpdateRequest {
        state,
        slug,
        id,
        form_data,
        file,
        auth_user,
        hx,
    } = req;

    let Some(def) = state.infra.registry.get_collection(slug).cloned() else {
        return redirect_response(paths::COLLECTIONS_ROOT).into_response();
    };

    // Parsed against the fields the edit form rendered for this viewer: an
    // input it never rendered is absent from the write, not an unchecked box.
    let locale = form_data.get("_locale").map(String::as_str);
    let form_fields = collection_form_fields(state, &def, id, auth_user, locale).await;
    let mut form = FormData::from_raw(form_data, &form_fields);

    let action = form.take_action();
    let draft = action == "save_draft";

    // Kept past the write: an error re-render has to put `_locale` back into
    // the form, or the corrected save lands in the default locale.
    let submitted_locale = form.take_locale();
    let locale_ctx = match parse_request_locale(submitted_locale.as_deref(), &state.config.locale) {
        Ok(ctx) => ctx,
        Err(msg) => return toast_only_error(&msg),
    };

    // The revision the form was loaded at: the save is refused when someone
    // else saved the document since. Kept for the error re-render too, so a
    // corrected save is still checked against it.
    let expected_revision = match form.take_revision() {
        Ok(revision) => revision,
        Err(msg) => return toast_only_error(&msg),
    };

    // Field and collection write access, and the password policy, are checked
    // inside the service write — a violation comes back as a `password` field
    // error the form re-render shows in the viewer's locale.
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

    let form_for_error = form.clone();
    let submitted_action = action.clone();

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
            expected_revision,
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
                meta: SubmittedMeta::builder()
                    .locale(submitted_locale.as_deref())
                    .locked(submitted_lock)
                    .revision(expected_revision)
                    .action(Some(submitted_action.as_str()))
                    .build(),
                hx,
            })
            .await
        }
        Err(e) => {
            error!("Update task error: {}", e);
            redirect_response(&paths::collection_item(slug, id))
        }
    }
}
