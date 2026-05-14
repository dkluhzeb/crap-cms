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
        Document, FieldError, ReqContext, SharedStorage, ValidationError,
        upload::{
            CleanupGuard, UploadedFile, delete_upload_files, enqueue_conversions,
            inject_upload_metadata, process_upload,
        },
    },
    db::{LocaleContext, query},
    service::{ServiceContext, WriteInput, create_document, update_document},
};

use super::ServiceError;

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
            .build(),
    )?;

    guard.commit();

    if !queued_conversions.is_empty()
        && let Some(pool) = ctx.pool
        && let Ok(conn) = pool.get()
        && let Err(e) = enqueue_conversions(&conn, ctx.slug, &doc.id, &queued_conversions)
    {
        warn!("Failed to enqueue image conversions: {}", e);
    }

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
    let def = ctx.collection_def()?;
    let locale_ctx = LocaleContext::from_locale_string(None, locale_config)?;

    // Load old document for file cleanup (before processing new file)
    let old_doc_fields = if file.is_some() {
        ctx.pool
            .and_then(|p| p.get().ok())
            .and_then(|conn| query::find_by_id(&conn, ctx.slug, def, id, locale_ctx.as_ref()).ok())
            .flatten()
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
    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let (doc, req_context) = update_document(
        ctx,
        id,
        WriteInput::builder(FormData::from_raw(form_data, &def.fields))
            .password(password.as_deref())
            .draft(draft)
            .ui_locale(ui_locale)
            .build(),
    )?;

    if let Some(mut g) = upload_guard {
        g.commit();
    }

    if let Some(old_fields) = old_doc_fields {
        delete_upload_files(&**storage, &old_fields);
    }

    if !queued_conversions.is_empty()
        && let Some(pool) = ctx.pool
        && let Ok(conn) = pool.get()
        && let Err(e) = enqueue_conversions(&conn, ctx.slug, &doc.id, &queued_conversions)
    {
        warn!("Failed to enqueue image conversions: {}", e);
    }

    Ok(UploadUpdateResult { doc, req_context })
}
