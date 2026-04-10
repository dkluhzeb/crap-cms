//! Upload service — file processing + document lifecycle for upload collections.
//!
//! Owns the full upload flow: process file → inject metadata → create/update document →
//! commit guard → clean up old files → enqueue conversions. Surfaces only handle
//! multipart parsing, auth, and response formatting.

use std::collections::HashMap;

use anyhow::anyhow;
use serde_json::Value;
use tracing::warn;

use crate::{
    admin::handlers::forms::extract_join_data_from_form,
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document,
        upload::{
            CleanupGuard, SharedStorage, UploadedFile, delete_upload_files, enqueue_conversions,
            inject_upload_metadata, process_upload,
        },
        validate::{FieldError, ValidationError},
    },
    db::{DbPool, LocaleContext, query},
    hooks::HookRunner,
    service::{WriteInput, create_document, update_document},
};

use super::ServiceError;

/// Result of a successful upload-create operation.
pub struct UploadCreateResult {
    pub doc: Document,
    pub req_context: HashMap<String, Value>,
}

/// Result of a successful upload-update operation.
pub struct UploadUpdateResult {
    pub doc: Document,
    pub req_context: HashMap<String, Value>,
}

/// Process a file and create an upload document.
///
/// Full lifecycle: process file → inject metadata → create document → commit guard → enqueue conversions.
/// The caller is responsible for multipart parsing and auth — this function takes the parsed file and form data.
#[allow(clippy::too_many_arguments)]
pub fn create_upload(
    pool: &DbPool,
    runner: &HookRunner,
    storage: &SharedStorage,
    slug: &str,
    def: &CollectionDefinition,
    file: UploadedFile,
    mut form_data: HashMap<String, String>,
    user: Option<&Document>,
    ui_locale: Option<String>,
    upload_max_file_size: u64,
) -> Result<UploadCreateResult, ServiceError> {
    let upload_config = def
        .upload
        .clone()
        .ok_or_else(|| ServiceError::Internal(anyhow!("Upload config missing")))?;

    let (processed, mut guard) = process_upload(
        file,
        &upload_config,
        storage.clone(),
        slug,
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
    let join_data = extract_join_data_from_form(&form_data, &def.fields);
    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let (doc, req_context) = create_document(
        pool,
        runner,
        slug,
        def,
        WriteInput::builder(form_data, &join_data)
            .password(password.as_deref())
            .draft(draft)
            .ui_locale(ui_locale)
            .build(),
        user,
        false,
    )?;

    guard.commit();

    if !queued_conversions.is_empty()
        && let Ok(conn) = pool.get()
        && let Err(e) = enqueue_conversions(&conn, slug, &doc.id, &queued_conversions)
    {
        warn!("Failed to enqueue image conversions: {}", e);
    }

    Ok(UploadCreateResult { doc, req_context })
}

/// Process a file (optional) and update an upload document.
///
/// Full lifecycle: load old doc → process file → inject metadata → update document →
/// commit guard → delete old files → enqueue conversions.
#[allow(clippy::too_many_arguments)]
pub fn update_upload(
    pool: &DbPool,
    runner: &HookRunner,
    storage: &SharedStorage,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    file: Option<UploadedFile>,
    mut form_data: HashMap<String, String>,
    user: Option<&Document>,
    ui_locale: Option<String>,
    locale_config: &LocaleConfig,
    upload_max_file_size: u64,
) -> Result<UploadUpdateResult, ServiceError> {
    let locale_ctx = LocaleContext::from_locale_string(None, locale_config)?;

    // Load old document for file cleanup (before processing new file)
    let old_doc_fields = if file.is_some() {
        pool.get()
            .ok()
            .and_then(|conn| query::find_by_id(&conn, slug, def, id, locale_ctx.as_ref()).ok())
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
        let (processed, guard) = process_upload(
            f,
            &upload_config,
            storage.clone(),
            slug,
            upload_max_file_size,
        )
        .map_err(|e| {
            ServiceError::Validation(ValidationError::new(vec![FieldError::new(
                "_file",
                e.to_string(),
            )]))
        })?;

        queued_conversions = processed.queued_conversions.clone();
        upload_guard = Some(guard);
        inject_upload_metadata(&mut form_data, &processed);
    }

    let password = if def.is_auth_collection() {
        form_data.remove("password")
    } else {
        None
    };
    let join_data = extract_join_data_from_form(&form_data, &def.fields);
    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let (doc, req_context) = update_document(
        pool,
        runner,
        slug,
        id,
        def,
        WriteInput::builder(form_data, &join_data)
            .password(password.as_deref())
            .draft(draft)
            .ui_locale(ui_locale)
            .build(),
        user,
        false,
    )?;

    if let Some(mut g) = upload_guard {
        g.commit();
    }

    if let Some(old_fields) = old_doc_fields {
        delete_upload_files(&**storage, &old_fields);
    }

    if !queued_conversions.is_empty()
        && let Ok(conn) = pool.get()
        && let Err(e) = enqueue_conversions(&conn, slug, &doc.id, &queued_conversions)
    {
        warn!("Failed to enqueue image conversions: {}", e);
    }

    Ok(UploadUpdateResult { doc, req_context })
}
