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

use anyhow::anyhow;

use crate::{
    admin::{FormData, strip_locale_locked_form_fields},
    core::{
        CollectionDefinition, Document, DocumentFields, FieldError, ReqContext, SharedStorage,
        ValidationError,
        upload::{
            CleanupGuard, QueuedConversion, UploadedFile, inject_upload_metadata, process_upload,
        },
    },
    db::LocaleContext,
    service::{
        ServiceContext, UploadConversions, WriteInput, create_document, op::reject_all_locales,
        orchestrate::CommitWatch, update_document,
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

/// Where a file goes and how large it may be.
struct FileStore<'a> {
    storage: &'a SharedStorage,
    max_file_size: u64,
}

impl<'a> FileStore<'a> {
    fn new(storage: &'a SharedStorage, max_file_size: u64) -> Self {
        Self {
            storage,
            max_file_size,
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

/// Store the file and inject its server-derived metadata into `form`.
///
/// Returns the guard that removes the stored bytes unless the write commits,
/// together with the conversions the file queued.
///
/// # Errors
///
/// A rejected file (type, size, unreadable image) is a `_file` validation
/// error, so every surface can render it against the form's file input.
fn store_file(
    ctx: &ServiceContext,
    store: &FileStore<'_>,
    file: &UploadedFile,
    form: &mut FormData,
) -> Result<(CleanupGuard, Vec<QueuedConversion>)> {
    let def = ctx.collection_def()?;

    let upload_config = def
        .upload
        .clone()
        .ok_or_else(|| ServiceError::Internal(anyhow!("Upload config missing")))?;

    let (processed, guard) = process_upload(
        file,
        &upload_config,
        store.storage,
        ctx.slug,
        store.max_file_size,
    )
    .map_err(|e| {
        ServiceError::Validation(ValidationError::new(vec![FieldError::new(
            "_file",
            e.to_string(),
        )]))
    })?;

    inject_upload_metadata(form.raw_mut(), &processed, &upload_config);

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
    pub ui_locale: Option<String>,
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
    pub ui_locale: Option<String>,
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
    let def = ctx.collection_def()?;

    // A file-bearing write reaches `create_document` without an operation, so
    // the rule every other write gets from the operation body is applied here.
    reject_all_locales(input.locale_ctx)?;

    strip_derived_columns(&mut input.form, def);

    let store = FileStore::new(input.storage, input.upload_max_file_size);
    let (guard, conversions) = store_file(ctx, &store, input.file, &mut input.form)?;
    let stored = StoredFile::new(guard);

    let (doc, req_context) = create_document(
        ctx,
        WriteInput::builder(input.form)
            .password(input.password.as_deref())
            .locale_ctx(input.locale_ctx)
            .draft(input.draft)
            .ui_locale(input.ui_locale)
            .trusted_upload_metadata(true)
            .upload_conversions(Some(UploadConversions::new(
                conversions,
                input.image_max_attempts,
            )))
            .build(),
    )?;

    // The row is written; an error above dropped `stored` with no commit
    // on its watch and took the bytes with it.
    stored.keep();

    Ok(UploadCreateResult { doc, req_context })
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
    input: UpdateUploadInput<'_>,
) -> Result<UploadUpdateResult> {
    let def = ctx.collection_def()?;

    let UpdateUploadInput {
        id,
        storage,
        file,
        mut form,
        locale_ctx,
        password,
        ui_locale,
        draft,
        upload_max_file_size,
        image_max_attempts,
        form_echoes_locked_fields,
    } = input;

    // See `create_upload`: the same rejection, applied where the write is.
    reject_all_locales(locale_ctx)?;

    strip_derived_columns(&mut form, def);

    let store = FileStore::new(storage, upload_max_file_size);

    let stored = match file.as_ref() {
        Some(file) => Some(store_file(ctx, &store, file, &mut form)?),
        None => None,
    };

    let (stored, conversions) = match stored {
        Some((guard, queued)) => (
            Some(StoredFile::new(guard)),
            Some(UploadConversions::new(queued, image_max_attempts)),
        ),
        None => (None, None),
    };

    let mut data: DocumentFields = form.into();

    if form_echoes_locked_fields {
        data = strip_locale_locked_form_fields(data, &def.fields, locale_ctx);
    }

    let (doc, req_context) = update_document(
        ctx,
        id,
        WriteInput::builder(data)
            .password(password.as_deref())
            .locale_ctx(locale_ctx)
            .draft(draft)
            .ui_locale(ui_locale)
            .trusted_upload_metadata(true)
            .upload_conversions(conversions)
            .build(),
    )?;

    if let Some(stored) = stored {
        stored.keep();
    }

    Ok(UploadUpdateResult { doc, req_context })
}

#[cfg(all(test, feature = "sqlite"))]
mod tests;
