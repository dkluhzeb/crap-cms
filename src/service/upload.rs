//! Upload service — the ONE entry every upload write goes through.
//!
//! Owns the file half of an upload write: store the file, inject the
//! server-derived metadata, hand the write the conversions the file queued, and
//! release the stored bytes only once the write has returned. Every surface
//! (REST multipart, the admin edit form) reaches a write on an upload
//! collection through here; what happens to the *previous* file — its bytes and
//! the conversions still queued for it — is settled inside the write
//! transaction by `service::write::settle_upload_write`, so the rule is decided
//! once rather than per surface.
//!
//! Surfaces keep what is theirs: multipart parsing, auth, CSRF, and response
//! formatting.

use anyhow::{Context as _, Error, anyhow};

use crate::{
    admin::{FormData, strip_locale_locked_form_fields},
    core::{
        CollectionDefinition, Document, DocumentFields, FieldError, ReqContext, SharedStorage,
        ValidationError,
        upload::{
            CleanupGuard, CollectionUpload, InspectedUpload, QueuedConversion, UploadedFile,
            inject_upload_metadata, inspect_upload, process_upload,
        },
    },
    db::LocaleContext,
    service::{
        RunnerWriteHooks, ServiceContext, UploadConversions, WriteHooks, WriteInput,
        WriteInputBuilder, admit_create_input, admit_update_input, check_create_access,
        check_update_access, create_document, op::reject_all_locales, orchestrate::CommitWatch,
        update_document,
    },
};

use super::ServiceError;

type Result<T> = std::result::Result<T, ServiceError>;

/// The stored bytes of one upload write, kept exactly when the row naming
/// them is durable.
///
/// The cleanup guard deletes the bytes unless it is committed. That commit is
/// keyed on the write's commit watch, not only on its return value: the write
/// envelope runs post-commit work (cache clear, event publishing, effect
/// flushes) before it returns, and a panic in that work unwinds through this
/// scope. Keyed on the return value alone, that unwind deleted a file the
/// committed row already pointed at; the watch keeps the bytes whenever the
/// commit happened, and a write that never committed still takes them with it.
struct StoredFile {
    guard: CleanupGuard,
    watch: CommitWatch,
}

impl StoredFile {
    /// Opens the watch BEFORE the write runs, so its commit cannot slip past.
    fn new(guard: CleanupGuard) -> Self {
        Self {
            guard,
            watch: CommitWatch::open(),
        }
    }

    /// The write returned successfully: the bytes stay. Also covers a caller
    /// that owns its own transaction, where no pool-write commit is watched.
    fn keep(mut self) {
        self.guard.commit();
    }
}

impl Drop for StoredFile {
    fn drop(&mut self) {
        if self.watch.committed() {
            self.guard.commit();
        }
    }
}

/// Drop every caller-supplied server-derived upload column before the real ones
/// are injected, so a forged `url`/`*_url` (including a not-yet-processed
/// queued-format size) can never survive even on this trusted, file-bearing
/// path. On a no-file update their absence means "keep what is stored".
fn strip_derived_columns(form: &mut FormData, def: &CollectionDefinition) {
    let Some(upload) = def.upload.as_ref() else {
        return;
    };

    for name in upload.derived_field_names() {
        form.take(&name);
    }
}

/// The collection-level gate of the write a file is stored for — `update` of
/// `id`, or `create` without one — judged by `write_hooks`.
fn judge_write_access(
    ctx: &ServiceContext,
    write_hooks: &dyn WriteHooks,
    id: Option<&str>,
    probe: &WriteInput<'_>,
) -> Result<()> {
    let def = ctx.collection_def()?;
    let locale = probe.locale_ctx.map(LocaleContext::access_locale);

    match id {
        Some(id) => check_update_access(ctx, write_hooks, def, id, &probe.data, locale),
        None => check_create_access(ctx, write_hooks, def, &probe.data, locale),
    }
}

/// [`judge_write_access`] in a transaction that is always rolled back, with the
/// runner's write hooks bound to it — the pool-mode caller has no hooks yet.
fn judge_in_rolled_back_tx(
    ctx: &ServiceContext,
    id: Option<&str>,
    probe: &WriteInput<'_>,
) -> Result<()> {
    let pool = ctx.pool.context("pool required")?;
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn
        .transaction()
        .context("Start upload access pre-check transaction")?;

    let mut write_hooks = RunnerWriteHooks::new(ctx.runner()?).with_conn(&tx);
    if ctx.override_access {
        write_hooks = write_hooks.with_override_access();
    }

    let verdict = judge_write_access(ctx, &write_hooks, id, probe);

    // A pre-check writes nothing.
    drop(tx);

    verdict
}

/// Refuse a caller the collection's `create` / `update` rule denies BEFORE the
/// file is stored.
///
/// Storing runs the whole image pipeline — decode, every resize, the
/// synchronous format conversions — and writes the bytes. Judged only by the
/// write afterwards, a caller with no write access at all could still make the
/// server do every bit of that per request. The gate is the write's own
/// chokepoint, on the request's fields admitted exactly as the write admits
/// them, together with the file's own columns ([`probe_form`]) — so a rule
/// reading `ctx.data.mime_type` judges the file it is about to let in. The
/// write still runs its own gate on the final data.
fn precheck_write_access(
    ctx: &ServiceContext,
    id: Option<&str>,
    mut probe: WriteInput<'_>,
) -> Result<()> {
    let def = ctx.collection_def()?;

    match id {
        Some(id) => {
            admit_update_input(ctx, def, id, &mut probe)?;
        }
        None => admit_create_input(def, &mut probe)?,
    }

    let Some(write_hooks) = ctx.write_hooks else {
        return judge_in_rolled_back_tx(ctx, id, &probe);
    };

    judge_write_access(ctx, write_hooks, id, &probe)
}

/// The collection's upload config.
fn upload_config(def: &CollectionDefinition) -> Result<&CollectionUpload> {
    def.upload
        .as_ref()
        .ok_or_else(|| ServiceError::Internal(anyhow!("Upload config missing")))
}

/// A refused or unprocessable file as a `_file` validation error, so every
/// surface can render it against the form's file input.
fn file_error(e: &Error) -> ServiceError {
    ServiceError::Validation(ValidationError::new(vec![FieldError::new(
        "_file",
        e.to_string(),
    )]))
}

/// Validate `file` for the collection and derive the columns it will be stored
/// with. Nothing is stored.
///
/// # Errors
///
/// A rejected file (type, size, unreadable image) is a `_file` validation
/// error.
fn inspect_file<'a>(
    upload: &CollectionUpload,
    file: &'a UploadedFile,
    max_file_size: u64,
) -> Result<InspectedUpload<'a>> {
    inspect_upload(file, upload, max_file_size).map_err(|e| file_error(&e))
}

/// `form` as the write will receive it once `inspected` is stored — its
/// derivable columns (`filename`, `mime_type`, `filesize`, and `width` /
/// `height` for an image) injected exactly as the write injects them — for the
/// access pre-check. The columns that only exist once the bytes are stored
/// (`url` and every per-size column) are blank, as a file without them is.
fn probe_form(
    form: &FormData,
    inspected: &InspectedUpload<'_>,
    upload: &CollectionUpload,
) -> FormData {
    let mut probe = form.clone();
    inspected.columns().inject(probe.raw_mut(), upload);

    probe
}

/// Store the inspected file and inject its server-derived metadata into
/// `form`.
///
/// Returns the guard that removes the stored bytes unless the write commits,
/// together with the conversions the file queued.
///
/// # Errors
///
/// A file that cannot be stored or decoded is a `_file` validation error.
fn store_file(
    ctx: &ServiceContext,
    storage: &SharedStorage,
    inspected: InspectedUpload<'_>,
    form: &mut FormData,
) -> Result<(CleanupGuard, Vec<QueuedConversion>)> {
    let upload = upload_config(ctx.collection_def()?)?;

    let (processed, guard) =
        process_upload(inspected, upload, storage, ctx.slug).map_err(|e| file_error(&e))?;

    inject_upload_metadata(form.raw_mut(), &processed, upload);

    Ok((guard, processed.queued_conversions))
}

/// Result of a successful upload-create operation.
pub struct UploadCreateResult {
    pub doc: Document,
    pub req_context: ReqContext,
}

/// Result of a successful upload-update operation.
pub struct UploadUpdateResult {
    pub doc: Document,
    pub req_context: ReqContext,
}

/// Input for [`create_upload`].
pub struct CreateUploadInput<'a> {
    pub storage: &'a SharedStorage,
    pub file: &'a UploadedFile,
    pub form: FormData,
    /// The locale the row is written in. A create is always in the default
    /// locale (the write rejects any other), but a localized collection still
    /// needs the context to address `title__en` rather than a bare `title`.
    pub locale_ctx: Option<&'a LocaleContext>,
    pub password: Option<String>,
    pub draft: bool,
    pub upload_max_file_size: u64,
    /// `max_attempts` the queued conversions are inserted with. Derived from
    /// `JobsConfig::system_image_max_attempts()` at the surface; tests can pass
    /// [`crate::core::upload::FALLBACK_MAX_ATTEMPTS`].
    pub image_max_attempts: u32,
}

/// Input for [`update_upload`].
pub struct UpdateUploadInput<'a> {
    pub id: &'a str,
    pub storage: &'a SharedStorage,
    pub file: Option<UploadedFile>,
    pub form: FormData,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub password: Option<String>,
    pub draft: bool,
    pub upload_max_file_size: u64,
    /// See [`CreateUploadInput::image_max_attempts`].
    pub image_max_attempts: u32,
    /// The data came from an HTML edit form, which round-trips shared
    /// (non-localized) fields as read-only inputs. Under a non-default locale a
    /// publish then drops them instead of failing the write, exactly as the
    /// draft path already does. A programmatic caller leaves this false: it
    /// spelled such a field out deliberately, so the write still rejects it
    /// rather than silently discarding the value.
    pub form_echoes_locked_fields: bool,
}

/// The builder of an upload write on `data`: the request's locale and draft
/// flag, with the server-derived upload columns trusted (they
/// were injected from the inspected file, never taken from the caller).
fn upload_write(
    data: impl Into<DocumentFields>,
    locale_ctx: Option<&LocaleContext>,
    draft: bool,
) -> WriteInputBuilder<'_> {
    WriteInput::builder(data)
        .locale_ctx(locale_ctx)
        .draft(draft)
        .trusted_upload_metadata(true)
}

/// Inspect the create's file, pre-check the write's access on the columns it
/// determines, then store it — the bytes never land for a write the
/// collection's rule refuses.
fn store_create_file(
    ctx: &ServiceContext,
    input: &mut CreateUploadInput<'_>,
) -> Result<(StoredFile, UploadConversions)> {
    let upload = upload_config(ctx.collection_def()?)?;
    let inspected = inspect_file(upload, input.file, input.upload_max_file_size)?;

    let probe = probe_form(&input.form, &inspected, upload);
    let precheck = upload_write(probe, input.locale_ctx, input.draft);
    precheck_write_access(ctx, None, precheck.build())?;

    let (guard, queued) = store_file(ctx, input.storage, inspected, &mut input.form)?;

    Ok((
        StoredFile::new(guard),
        UploadConversions::new(queued, input.image_max_attempts),
    ))
}

/// Process a file and create an upload document.
///
/// # Errors
///
/// Returns a `ValidationError` if the write names the all-locales mode or file
/// processing fails, or any service-layer error from the underlying
/// `create_document` (access denied, validation, …).
pub fn create_upload(
    ctx: &ServiceContext,
    mut input: CreateUploadInput<'_>,
) -> Result<UploadCreateResult> {
    // A file-bearing write reaches `create_document` without an operation, so
    // the rule every other write gets from the operation body is applied here.
    reject_all_locales(input.locale_ctx)?;

    strip_derived_columns(&mut input.form, ctx.collection_def()?);

    let (stored, conversions) = store_create_file(ctx, &mut input)?;

    let write = upload_write(input.form, input.locale_ctx, input.draft)
        .password(input.password.as_deref())
        .upload_conversions(Some(conversions));
    let (doc, req_context) = create_document(ctx, write.build())?;

    // The row is written; an error above dropped `stored` with no commit
    // on its watch and took the bytes with it.
    stored.keep();

    Ok(UploadCreateResult { doc, req_context })
}

/// The data an update writes from `form`: an HTML edit form's echoed shared
/// fields dropped under a non-default locale (see
/// [`UpdateUploadInput::form_echoes_locked_fields`]).
fn update_data(
    form: FormData,
    def: &CollectionDefinition,
    locale_ctx: Option<&LocaleContext>,
    form_echoes_locked_fields: bool,
) -> DocumentFields {
    let data: DocumentFields = form.into();

    if !form_echoes_locked_fields {
        return data;
    }

    strip_locale_locked_form_fields(data, &def.fields, locale_ctx)
}

/// The access pre-check's input for an update storing `inspected`: the form
/// with the file's own columns, admitted as [`update_data`] admits the write's.
fn update_precheck<'a>(
    input: &'a UpdateUploadInput<'_>,
    def: &CollectionDefinition,
    inspected: &InspectedUpload<'_>,
    upload: &CollectionUpload,
) -> WriteInput<'a> {
    let probe = probe_form(&input.form, inspected, upload);
    let probe = update_data(
        probe,
        def,
        input.locale_ctx,
        input.form_echoes_locked_fields,
    );

    upload_write(probe, input.locale_ctx, input.draft).build()
}

/// Inspect the update's replacement file (if it carries one), pre-check the
/// write's access on the columns it determines, then store it — the bytes
/// never land for a write the collection's rule refuses. `None` when the
/// update carries no file.
fn store_update_file(
    ctx: &ServiceContext,
    input: &mut UpdateUploadInput<'_>,
) -> Result<Option<(StoredFile, UploadConversions)>> {
    let Some(file) = input.file.take() else {
        return Ok(None);
    };

    let def = ctx.collection_def()?;
    let upload = upload_config(def)?;
    let inspected = inspect_file(upload, &file, input.upload_max_file_size)?;

    let precheck = update_precheck(input, def, &inspected, upload);
    precheck_write_access(ctx, Some(input.id), precheck)?;

    let (guard, queued) = store_file(ctx, input.storage, inspected, &mut input.form)?;

    Ok(Some((
        StoredFile::new(guard),
        UploadConversions::new(queued, input.image_max_attempts),
    )))
}

/// Process a file (optional) and update an upload document.
///
/// # Errors
///
/// Returns a `ValidationError` if the write names the all-locales mode or file
/// processing fails, or any service-layer error from the underlying
/// `update_document` (access denied, validation, …).
pub fn update_upload(
    ctx: &ServiceContext,
    mut input: UpdateUploadInput<'_>,
) -> Result<UploadUpdateResult> {
    let def = ctx.collection_def()?;

    // See `create_upload`: the same rejection, applied where the write is.
    reject_all_locales(input.locale_ctx)?;

    strip_derived_columns(&mut input.form, def);

    let (stored, conversions) = store_update_file(ctx, &mut input)?.unzip();

    let data = update_data(
        input.form,
        def,
        input.locale_ctx,
        input.form_echoes_locked_fields,
    );
    let write = upload_write(data, input.locale_ctx, input.draft)
        .password(input.password.as_deref())
        .upload_conversions(conversions);
    let (doc, req_context) = update_document(ctx, input.id, write.build())?;

    if let Some(stored) = stored {
        stored.keep();
    }

    Ok(UploadUpdateResult { doc, req_context })
}

#[cfg(all(test, feature = "sqlite"))]
mod tests;
