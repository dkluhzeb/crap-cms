//! Shared upload processing for collection create/update handlers.

use std::collections::HashMap;

use axum::{Extension, response::Response};
use serde_json::Value;
use tokio::task;
use tracing::error;

use crate::{
    admin::AdminState,
    core::{
        auth::AuthUser,
        collection::CollectionDefinition,
        upload::{
            CleanupGuard, QueuedConversion, UploadedFile, inject_upload_metadata, process_upload,
        },
    },
    db::query::{self, LocaleContext},
};

use super::{render_edit_upload_error, render_upload_error};

/// Upload processing result with optional old file fields for cleanup.
pub(in crate::admin::handlers::collections) struct UploadResult {
    pub queued_conversions: Vec<QueuedConversion>,
    pub guard: CleanupGuard,
    pub old_doc_fields: Option<HashMap<String, Value>>,
}

/// Parameters for upload processing.
pub(in crate::admin::handlers::collections) struct UploadParams<'a> {
    pub state: &'a AdminState,
    pub def: &'a CollectionDefinition,
    pub slug: &'a str,
    pub doc_id: Option<&'a str>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub auth_user: &'a Option<Extension<AuthUser>>,
}

/// Process a file upload in a blocking task, injecting metadata into form_data.
///
/// For updates (`doc_id = Some`), also loads the old document's fields so the
/// caller can clean up old files after a successful write. For creates (`doc_id = None`),
/// `old_doc_fields` is always `None`.
///
/// On error, renders the appropriate error page (create or edit mode).
pub(in crate::admin::handlers::collections) async fn process_collection_upload(
    p: &UploadParams<'_>,
    form_data: &mut HashMap<String, String>,
    file: UploadedFile,
) -> Result<UploadResult, Response> {
    let upload_config = p.def.upload.clone().expect("upload config required");

    // For updates, load old document to get old file paths for cleanup.
    // Internal lookup for file cleanup planning, not a user-facing read.
    let old_doc_fields = if let Some(id) = p.doc_id {
        p.state
            .pool
            .get()
            .ok()
            .and_then(|conn| query::find_by_id(&conn, p.slug, p.def, id, p.locale_ctx).ok())
            .flatten()
            .map(|doc| doc.fields.clone())
    } else {
        None
    };

    let storage = p.state.storage.clone();
    let slug_owned = p.slug.to_string();
    let global_max = p.state.config.upload.max_file_size;

    let result = task::spawn_blocking(move || {
        process_upload(file, &upload_config, storage, &slug_owned, global_max)
    })
    .await;

    match result {
        Ok(Ok((processed, guard))) => {
            inject_upload_metadata(form_data, &processed);

            Ok(UploadResult {
                queued_conversions: processed.queued_conversions.clone(),
                guard,
                old_doc_fields,
            })
        }
        Ok(Err(e)) => {
            error!("Upload processing error: {}", e);
            Err(render_error(
                p.state,
                p.def,
                form_data,
                p.doc_id,
                p.auth_user,
                &e.to_string(),
            ))
        }
        Err(e) => {
            error!("Upload task error: {}", e);
            Err(render_error(
                p.state,
                p.def,
                form_data,
                p.doc_id,
                p.auth_user,
                &e.to_string(),
            ))
        }
    }
}

/// Render the appropriate upload error page based on create/edit mode.
fn render_error(
    state: &AdminState,
    def: &CollectionDefinition,
    form_data: &HashMap<String, String>,
    doc_id: Option<&str>,
    auth_user: &Option<Extension<AuthUser>>,
    err_msg: &str,
) -> Response {
    if let Some(id) = doc_id {
        render_edit_upload_error(state, def, form_data, id, auth_user, err_msg)
    } else {
        render_upload_error(state, def, form_data, auth_user, err_msg)
    }
}
