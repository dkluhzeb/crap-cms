//! DELETE /api/upload/{slug}/{id} — delete an upload document and its files.

use tracing::error;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use tokio::task;

use crate::{
    admin::AdminState,
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document, ReqContext, SharedInvalidationTransport, SharedStorage,
        event::EventOperation,
    },
    db::{DbPool, LocaleContext, query},
    hooks::HookRunner,
    service::{ServiceContext, ServiceError, delete_document},
};

/// Owned bundle for the upload-delete spawn-blocking body.
struct UploadDeleteBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    def: CollectionDefinition,
    slug: String,
    id: String,
    user_doc: Option<Document>,
    storage: SharedStorage,
    locale_config: LocaleConfig,
    invalidation_transport: SharedInvalidationTransport,
}

fn delete_upload_blocking(input: UploadDeleteBlockingInput) -> Result<ReqContext, ServiceError> {
    let ctx = ServiceContext::collection(&input.slug, &input.def)
        .pool(&input.pool)
        .runner(&input.runner)
        .user(input.user_doc.as_ref())
        .invalidation_transport(Some(input.invalidation_transport))
        .build();

    delete_document(
        &ctx,
        &input.id,
        Some(&*input.storage),
        Some(&input.locale_config),
    )
}

use super::helpers::{
    SuccessBody, check_upload_access, classify_delete_error, extract_bearer_user, json_error,
    json_ok, publish_upload_event,
};

/// Existence precheck for `delete_upload`. Builds a proper locale context so the
/// SELECT targets locale-suffixed columns (`caption__en`) on localized
/// collections — a bare `None` locale generates unsuffixed column names and
/// errors, which the old `.ok().flatten()` swallowed into a spurious 404.
/// Distinguishes a genuine 404 (`Ok(None)`) from a backend error (surfaced as
/// 500 rather than reported to the client as "not found").
fn ensure_upload_exists(
    state: &AdminState,
    def: &CollectionDefinition,
    slug: &str,
    id: &str,
) -> Result<(), Box<Response>> {
    let internal = || {
        Box::new(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal error",
        ))
    };

    let locale_ctx = LocaleContext::from_locale_string(None, &state.config.locale)
        .inspect_err(|e| error!("Upload delete locale context error: {e}"))
        .map_err(|_| internal())?;

    let conn = state
        .pool
        .get()
        .inspect_err(|e| error!("Upload delete pool error: {e}"))
        .map_err(|_| internal())?;

    let doc = query::find_by_id(&conn, slug, def, id, locale_ctx.as_ref())
        .inspect_err(|e| error!("Upload delete existence check failed: {e}"))
        .map_err(|_| internal())?;

    if doc.is_none() {
        return Err(Box::new(json_error(
            StatusCode::NOT_FOUND,
            &format!("Document '{id}' not found"),
        )));
    }

    Ok(())
}

#[cfg(not(tarpaulin_include))]
pub(super) async fn delete_upload(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let auth_user = match extract_bearer_user(&state, &headers) {
        Ok(u) => u,
        Err(e) => return *e,
    };

    let Some(def) = state.registry.get_collection(&slug).cloned() else {
        return json_error(
            StatusCode::NOT_FOUND,
            &format!("Collection '{slug}' not found"),
        );
    };

    if !def.is_upload_collection() {
        return json_error(
            StatusCode::BAD_REQUEST,
            &format!("Collection '{slug}' is not an upload collection"),
        );
    }

    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
    let access_fn = if def.soft_delete {
        def.access.resolve_trash()
    } else {
        def.access.delete.as_ref()
    };

    if let Err(resp) = check_upload_access(
        &state,
        access_fn,
        user_doc,
        Some(&id),
        // Soft delete → "trash" (trash access fn); hard delete → "delete".
        if def.soft_delete {
            "Trash access denied"
        } else {
            "Delete access denied"
        },
        if def.soft_delete { "trash" } else { "delete" },
        &def.slug,
    ) {
        return *resp;
    }

    if let Err(resp) = ensure_upload_exists(&state, &def, &slug, &id) {
        return *resp;
    }

    let input = UploadDeleteBlockingInput {
        pool: state.pool.clone(),
        runner: state.hook_runner.clone(),
        def: def.clone(),
        slug: slug.clone(),
        id: id.clone(),
        user_doc: auth_user.as_ref().map(|au| au.user_doc.clone()),
        storage: state.storage.clone(),
        locale_config: state.config.locale.clone(),
        invalidation_transport: state.invalidation_transport.clone(),
    };

    let result = task::spawn_blocking(move || delete_upload_blocking(input)).await;

    match result {
        Ok(Ok(_req_context)) => {
            publish_upload_event(
                &state,
                &def,
                slug,
                id,
                EventOperation::Delete,
                None,
                auth_user.as_ref(),
            );
            json_ok(StatusCode::OK, &SuccessBody { success: true })
        }
        Ok(Err(e)) => {
            let msg = e.to_string();
            let status = classify_delete_error(&msg);

            // Log full detail internally; the client only learns the
            // status code and a generic phrase. Keeps internal DB errors,
            // stack traces, and backend identifiers off the wire.
            error!("Upload delete failed: {}", msg);

            let client_msg = match status {
                StatusCode::NOT_FOUND => "Upload not found",
                StatusCode::CONFLICT => "Upload is still referenced",
                _ => "Delete failed",
            };

            json_error(status, client_msg)
        }
        Err(e) => {
            error!("Upload delete task join failed: {}", e);

            json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        }
    }
}
