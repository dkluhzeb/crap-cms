//! DELETE /api/upload/{slug}/{id} — delete an upload document and its files.

use std::sync::Arc;

use tracing::error;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use tokio::task;

use crate::{
    admin::{AdminState, handlers::shared::response::on_blocking_section},
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
    def: Arc<CollectionDefinition>,
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

    // Recover the real error kind from a bare `Internal` before the HTTP mapper,
    // matching the gRPC/admin write paths (see `create_upload_blocking`).
    let db_kind = input.pool.kind();
    delete_document(
        &ctx,
        &input.id,
        Some(&*input.storage),
        Some(&input.locale_config),
    )
    .map_err(|e| e.reclassify(db_kind))
}

use super::helpers::{
    SuccessBody, check_upload_access, extract_bearer_user, json_error, json_ok,
    publish_upload_event, service_error_to_response,
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
        .infra
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
    // L12: the entire gate prologue — auth, the Lua access hook, and the
    // existence probe — is synchronous DB/VM work; run it on the blocking
    // pool instead of parking an async worker.
    let (auth_user, def) = match on_blocking_section(|| {
        let auth_user = extract_bearer_user(&state, &headers)?;

        let def = state
            .infra
            .registry
            .get_collection(&slug)
            .cloned()
            .ok_or_else(|| {
                Box::new(json_error(
                    StatusCode::NOT_FOUND,
                    &format!("Collection '{slug}' not found"),
                ))
            })?;

        if !def.is_upload_collection() {
            return Err(Box::new(json_error(
                StatusCode::BAD_REQUEST,
                &format!("Collection '{slug}' is not an upload collection"),
            )));
        }

        let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
        let access_fn = if def.soft_delete {
            def.access.resolve_trash()
        } else {
            def.access.delete.as_ref()
        };

        check_upload_access(
            &state,
            access_fn,
            user_doc,
            Some(&id),
            if def.soft_delete {
                "Trash access denied"
            } else {
                "Delete access denied"
            },
            if def.soft_delete { "trash" } else { "delete" },
            &def.slug,
        )?;

        ensure_upload_exists(&state, &def, &slug, &id)?;

        Ok((auth_user, def))
    }) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };

    let input = UploadDeleteBlockingInput {
        pool: state.infra.pool.clone(),
        runner: state.infra.hook_runner.clone(),
        def: def.clone(),
        slug: slug.clone(),
        id: id.clone(),
        user_doc: auth_user.as_ref().map(|au| au.user_doc.clone()),
        storage: state.infra.storage.clone(),
        locale_config: state.config.locale.clone(),
        invalidation_transport: state.infra.invalidation_transport.clone(),
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
        // One typed mapper for the whole upload surface (create/update/delete);
        // `Transient`/`Internal` are logged and reduced to a generic phrase
        // inside `service_error_to_response`.
        Ok(Err(e)) => service_error_to_response(&e),
        Err(e) => {
            error!("Upload delete task join failed: {}", e);

            json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error")
        }
    }
}
