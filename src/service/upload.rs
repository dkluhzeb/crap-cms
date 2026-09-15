//! Upload service — file processing + document lifecycle for upload collections.
//!
//! Owns the full upload flow: process file -> inject metadata -> create/update document ->
//! commit guard -> clean up old files -> enqueue conversions. Surfaces only handle
//! multipart parsing, auth, and response formatting.

use std::collections::HashMap;

use anyhow::anyhow;
use tracing::warn;

use crate::{
    admin::FormData,
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document, FieldError, ReqContext, SharedStorage, ValidationError,
        upload::{
            CleanupGuard, QueuedConversion, UploadedFile, delete_upload_files, enqueue_conversions,
            inject_upload_metadata, process_upload,
        },
    },
    db::{DbConnection, LocaleContext, query},
    service::{
        ServiceContext, WriteInput, create_document, update_document, write::cancel_image_jobs,
    },
};

use super::ServiceError;

/// What a committed upload write needs to settle the document's queued image
/// conversions.
struct ConversionRequeue<'a> {
    def: &'a CollectionDefinition,
    document_id: &'a str,
    /// The conversions the written file queued; empty when it needs none.
    conversions: &'a [QueuedConversion],
    max_attempts: u32,
    /// Whether this write replaced the document's file, so the conversions
    /// still queued for the previous one must be cancelled.
    replaced_file: bool,
}

/// Cancel the conversions still queued for a replaced file, then queue the
/// written file's.
///
/// A conversion queued for the previous file writes its derivative URL onto the
/// document when it runs; left in the queue across a file replacement it lands
/// after the new file's own conversions and overwrites them with a derivative
/// of a file the document no longer references. Best-effort, logged.
fn replace_queued_conversions(
    conn: &dyn DbConnection,
    slug: &str,
    requeue: &ConversionRequeue<'_>,
) {
    if requeue.replaced_file {
        cancel_image_jobs(conn, slug, requeue.def, requeue.document_id);
    }

    if requeue.conversions.is_empty() {
        return;
    }

    if let Err(e) = enqueue_conversions(
        conn,
        slug,
        requeue.document_id,
        requeue.conversions,
        requeue.max_attempts,
    ) {
        warn!("Failed to enqueue image conversions: {e}");
    }
}

/// [`replace_queued_conversions`] on a write connection from the context's
/// pool. A no-op without a pool (Lua CRUD), which queues no conversions.
fn requeue_conversions(ctx: &ServiceContext, requeue: &ConversionRequeue<'_>) {
    // Nothing to cancel and nothing to queue — don't take a write connection.
    if !requeue.replaced_file && requeue.conversions.is_empty() {
        return;
    }

    let Some(pool) = ctx.pool else {
        return;
    };

    let Ok(conn) = pool
        .write()
        .inspect_err(|e| warn!("Image conversions for {}: {e:#}", ctx.slug))
    else {
        return;
    };

    replace_queued_conversions(&conn, ctx.slug, requeue);
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

/// Process a file and create an upload document.
///
/// Full lifecycle: process file -> inject metadata -> create document -> commit guard -> enqueue conversions.
/// The caller is responsible for multipart parsing and auth — this function takes the parsed file and form data.
///
/// # Errors
///
/// Returns a `ValidationError` if file processing fails, or any service-layer
/// error from the underlying `create_document` (access denied, validation, etc.).
pub fn create_upload(
    ctx: &ServiceContext,
    storage: &SharedStorage,
    file: &UploadedFile,
    mut form_data: HashMap<String, String>,
    ui_locale: Option<String>,
    upload_max_file_size: u64,
    image_max_attempts: u32,
) -> Result<UploadCreateResult, ServiceError> {
    let def = ctx.collection_def()?;

    let upload_config = def
        .upload
        .clone()
        .ok_or_else(|| ServiceError::Internal(anyhow!("Upload config missing")))?;

    let (processed, mut guard) = process_upload(
        file,
        &upload_config,
        storage,
        ctx.slug,
        upload_max_file_size,
    )
    .map_err(|e| {
        ServiceError::Validation(ValidationError::new(vec![FieldError::new(
            "_file",
            e.to_string(),
        )]))
    })?;

    // Drop any caller-supplied server-derived columns before injecting the real
    // ones, so a forged `url`/`*_url` (incl. a not-yet-processed queued-format
    // size) can never survive even on this trusted, file-bearing path.
    for name in upload_config.derived_field_names() {
        form_data.remove(&name);
    }

    let queued_conversions = processed.queued_conversions.clone();
    inject_upload_metadata(&mut form_data, &processed);

    let password = if def.is_auth_collection() {
        form_data.remove("password")
    } else {
        None
    };
    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let (doc, req_context) = create_document(
        ctx,
        WriteInput::builder(FormData::from_raw(form_data, &def.fields))
            .password(password.as_deref())
            .draft(draft)
            .ui_locale(ui_locale)
            .trusted_upload_metadata(true)
            .build(),
    )?;

    guard.commit();

    // A create has no previous file, so nothing to cancel.
    requeue_conversions(
        ctx,
        &ConversionRequeue {
            def,
            document_id: &doc.id,
            conversions: &queued_conversions,
            max_attempts: image_max_attempts,
            replaced_file: false,
        },
    );

    Ok(UploadCreateResult { doc, req_context })
}

/// Input for [`update_upload`].
pub struct UpdateUploadInput<'a> {
    pub id: &'a str,
    pub storage: &'a SharedStorage,
    pub file: Option<UploadedFile>,
    pub form_data: HashMap<String, String>,
    pub ui_locale: Option<String>,
    pub locale_config: &'a LocaleConfig,
    pub upload_max_file_size: u64,
    /// `max_attempts` passed to `enqueue_conversions` when new image
    /// conversions are queued. Derived from
    /// `JobsConfig::system_image_max_attempts()` at the API/admin
    /// layer; tests can pass [`crate::core::upload::FALLBACK_MAX_ATTEMPTS`].
    pub image_max_attempts: u32,
}

/// Whether an upload update replaces the published document's file(s) — and so
/// must clean them up after commit. Only a **non-draft** write carrying a **new
/// file** does: a draft save leaves the published row (and its file references)
/// untouched, and a write with no new file changes no files. Deleting the old
/// file on a draft save would orphan the still-live published document.
fn replaces_published_files(has_new_file: bool, draft: bool) -> bool {
    has_new_file && !draft
}

/// Process a file (optional) and update an upload document.
///
/// Full lifecycle: load old doc -> process file -> inject metadata -> update document ->
/// commit guard -> delete old files -> enqueue conversions.
///
/// # Errors
///
/// Returns a `ValidationError` if file processing fails, or any service-layer
/// error from the underlying `update_document` (access denied, validation, etc.).
pub fn update_upload(
    ctx: &ServiceContext,
    input: UpdateUploadInput<'_>,
) -> Result<UploadUpdateResult, ServiceError> {
    let id = input.id;
    let storage = input.storage;
    let file = input.file;
    let mut form_data = input.form_data;
    let ui_locale = input.ui_locale;
    let locale_config = input.locale_config;
    let upload_max_file_size = input.upload_max_file_size;
    let image_max_attempts = input.image_max_attempts;
    let def = ctx.collection_def()?;
    let locale_ctx = LocaleContext::default_for(locale_config);

    // Strip caller-supplied server-derived upload columns up front: on a no-file
    // update they must stay unchanged (absent = keep stored), and on a file
    // update `inject_upload_metadata` sets the real values below. Prevents a
    // forged `url`/`*_url` from reaching the DB on either branch.
    if let Some(upload) = def.upload.as_ref() {
        for name in upload.derived_field_names() {
            form_data.remove(&name);
        }
    }

    // A draft save leaves the published row untouched — it still references the
    // current file — so on a draft the old files must NOT be cleaned up here.
    // Only a published update actually replaces them.
    let draft = form_data.remove("_action").unwrap_or_default() == "save_draft";

    // Load the published document's files for post-commit cleanup, but only when
    // this update replaces them (a new file on a non-draft write). A transient
    // read error must propagate: silently skipping cleanup leaks the previous
    // file and all its variants (the delete path is hardened the same way — the
    // old `.ok().flatten()` here was the swallow).
    let replaced_file = replaces_published_files(file.is_some(), draft);

    let old_doc_fields = if replaced_file {
        let pool = ctx.pool.ok_or_else(|| {
            ServiceError::Internal(anyhow!("a pool is required to replace an upload file"))
        })?;
        let conn = pool.get()?;

        query::find_by_id(&conn, ctx.slug, def, id, locale_ctx.as_ref())?
            .map(|doc| doc.fields.clone())
    } else {
        None
    };

    let mut queued_conversions = Vec::new();
    let mut upload_guard: Option<CleanupGuard> = None;

    if let Some(f) = file
        && let Some(upload_config) = def.upload.clone()
    {
        let (processed, guard) =
            process_upload(&f, &upload_config, storage, ctx.slug, upload_max_file_size).map_err(
                |e| {
                    ServiceError::Validation(ValidationError::new(vec![FieldError::new(
                        "_file",
                        e.to_string(),
                    )]))
                },
            )?;

        queued_conversions.clone_from(&processed.queued_conversions);
        upload_guard = Some(guard);
        inject_upload_metadata(&mut form_data, &processed);
    }

    let password = if def.is_auth_collection() {
        form_data.remove("password")
    } else {
        None
    };

    let (doc, req_context) = update_document(
        ctx,
        id,
        WriteInput::builder(FormData::from_raw(form_data, &def.fields))
            .password(password.as_deref())
            .draft(draft)
            .ui_locale(ui_locale)
            .trusted_upload_metadata(true)
            .build(),
    )?;

    if let Some(mut g) = upload_guard {
        g.commit();
    }

    if let Some(old_fields) = old_doc_fields
        && let Some(upload) = def.upload.as_ref()
    {
        delete_upload_files(&**storage, &old_fields, upload);
    }

    requeue_conversions(
        ctx,
        &ConversionRequeue {
            def,
            document_id: &doc.id,
            conversions: &queued_conversions,
            max_attempts: image_max_attempts,
            replaced_file,
        },
    );

    Ok(UploadUpdateResult { doc, req_context })
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::{
        core::upload::{
            CollectionUpload, FALLBACK_MAX_ATTEMPTS, ImageConvertJobData, queue_image_conversion,
        },
        db::{migrate, query::jobs::list_job_runs},
    };

    /// An in-memory database holding only the jobs table.
    fn jobs_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate::create_jobs_table(&conn, "TEXT DEFAULT (datetime('now'))", "TEXT")
            .expect("create_jobs_table");

        conn
    }

    fn media_upload_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("media");
        def.upload = Some(CollectionUpload::new());

        def
    }

    fn conversion(target: &str) -> QueuedConversion {
        QueuedConversion {
            source_path: "a.png".to_string(),
            target_path: target.to_string(),
            format: "webp".to_string(),
            quality: 80,
            url_column: "thumbnail_webp_url".to_string(),
            url_value: format!("/uploads/{target}"),
        }
    }

    /// The queued payloads of every job still in the queue.
    fn queued_payloads(conn: &Connection) -> Vec<String> {
        list_job_runs(conn, None, None, 100, 0)
            .unwrap()
            .into_iter()
            .map(|run| run.data)
            .collect()
    }

    /// Regression: replacing an upload's file queued the new conversions but
    /// left the previous file's queued. The stale run finishes later and writes
    /// its derivative URL over the new file's — the document then points at a
    /// derivative of a file it no longer references.
    #[test]
    fn replacing_a_file_cancels_the_previous_files_conversions() {
        let conn = jobs_db();
        let def = media_upload_def();

        queue_image_conversion(
            &conn,
            &ImageConvertJobData {
                collection: "media".to_string(),
                document_id: "m1".to_string(),
                source_path: "old.png".to_string(),
                target_path: "old.webp".to_string(),
                format: "webp".to_string(),
                quality: 80,
                url_column: "thumbnail_webp_url".to_string(),
                url_value: "/uploads/old.webp".to_string(),
            },
            FALLBACK_MAX_ATTEMPTS,
        )
        .unwrap();

        replace_queued_conversions(
            &conn,
            "media",
            &ConversionRequeue {
                def: &def,
                document_id: "m1",
                conversions: &[conversion("new.webp")],
                max_attempts: FALLBACK_MAX_ATTEMPTS,
                replaced_file: true,
            },
        );

        let payloads = queued_payloads(&conn);
        assert_eq!(payloads.len(), 1, "{payloads:?}");
        assert!(payloads[0].contains("new.webp"), "{payloads:?}");
    }

    /// A write that replaces no file (a create, or an update carrying no new
    /// file) leaves the document's queued conversions alone.
    #[test]
    fn a_write_that_replaces_no_file_keeps_the_queued_conversions() {
        let conn = jobs_db();
        let def = media_upload_def();

        queue_image_conversion(
            &conn,
            &ImageConvertJobData {
                collection: "media".to_string(),
                document_id: "m1".to_string(),
                source_path: "a.png".to_string(),
                target_path: "a.webp".to_string(),
                format: "webp".to_string(),
                quality: 80,
                url_column: "thumbnail_webp_url".to_string(),
                url_value: "/uploads/a.webp".to_string(),
            },
            FALLBACK_MAX_ATTEMPTS,
        )
        .unwrap();

        replace_queued_conversions(
            &conn,
            "media",
            &ConversionRequeue {
                def: &def,
                document_id: "m1",
                conversions: &[],
                max_attempts: FALLBACK_MAX_ATTEMPTS,
                replaced_file: false,
            },
        );

        assert_eq!(queued_payloads(&conn).len(), 1);
    }

    /// Regression: a draft save with a new file must NOT clean up the published
    /// file. The draft write leaves the published row pointing at the current
    /// file, so deleting it here orphaned the live document (broken image).
    #[test]
    fn draft_save_with_new_file_keeps_published_files() {
        // published update replacing the file → clean up the old file
        assert!(replaces_published_files(true, false));

        // draft save with a new file → the published file stays referenced
        assert!(!replaces_published_files(true, true));

        // no new file → nothing to replace, on either path
        assert!(!replaces_published_files(false, false));
        assert!(!replaces_published_files(false, true));
    }
}
