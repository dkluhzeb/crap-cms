//! DELETE /api/upload/{slug}/{id} — delete an upload document and its files.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use tokio::task;
use tracing::error;

use crate::{
    admin::{AdminState, handlers::shared::response::on_blocking_section},
    core::{CollectionDefinition, Document, ReqContext},
    service::{AppInfra, ServiceContext, ServiceError, delete_document},
};

use super::helpers::{
    SuccessBody, check_upload_access, extract_bearer_user, json_error, json_ok,
    service_error_to_response,
};

/// Owned bundle for the upload-delete spawn-blocking body. Storage, locale
/// config and transports come from `infra`.
struct UploadDeleteBlockingInput {
    infra: Arc<AppInfra>,
    def: Arc<CollectionDefinition>,
    slug: String,
    id: String,
    user_doc: Option<Document>,
}

fn delete_upload_blocking(input: &UploadDeleteBlockingInput) -> Result<ReqContext, ServiceError> {
    let ctx = ServiceContext::collection(&input.slug, &input.def)
        .infra(&input.infra)
        .user(input.user_doc.as_ref())
        .build();

    // Recover the real error kind from a bare `Internal` before the HTTP mapper,
    // matching the gRPC/admin write paths (see `create_upload_blocking`).
    let db_kind = input.infra.pool.kind();

    delete_document(
        &ctx,
        &input.id,
        Some(&*input.infra.storage),
        Some(&input.infra.locale_config),
    )
    .map_err(|e| e.reclassify(db_kind))
}

#[cfg(not(tarpaulin_include))]
pub(super) async fn delete_upload(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    // The entire gate prologue — auth, the Lua access hook, and the existence
    // probe — is synchronous DB/VM work; run it on the blocking pool instead of
    // parking an async worker.
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

        Ok((auth_user, def))
    }) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };

    let input = UploadDeleteBlockingInput {
        infra: state.infra.clone(),
        def: def.clone(),
        slug: slug.clone(),
        id: id.clone(),
        user_doc: auth_user.as_ref().map(|au| au.user_doc.clone()),
    };

    let result = task::spawn_blocking(move || delete_upload_blocking(&input)).await;

    match result {
        Ok(Ok(_req_context)) => json_ok(StatusCode::OK, &SuccessBody { success: true }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::upload::helpers::test_support, core::event::EventOperation, db::DbConnection,
    };

    /// An upload deleted through the REST API is removed by the service with
    /// the app infra: one delete event and a cleared populate cache.
    #[test]
    fn an_api_upload_is_deleted_through_the_service() {
        let (_tmp, infra, mut rx) = test_support::infra_with_events();
        let def = infra.registry.get_collection("media").cloned().unwrap();

        infra
            .pool
            .get()
            .unwrap()
            .execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();

        let input = UploadDeleteBlockingInput {
            infra: Arc::clone(&infra),
            def,
            slug: "media".to_string(),
            id: "m1".to_string(),
            user_doc: None,
        };

        delete_upload_blocking(&input).expect("delete upload");

        assert!(
            !infra.cache.has(test_support::CACHED_KEY).unwrap(),
            "the populate cache must be cleared"
        );

        let event = rx.try_recv().expect("a delete event");
        assert!(matches!(event.operation, EventOperation::Delete));
        assert!(rx.try_recv().is_err(), "exactly one event");

        let remaining = infra
            .pool
            .get()
            .unwrap()
            .query_one("SELECT id FROM media WHERE id = 'm1'", &[])
            .unwrap();
        assert!(remaining.is_none(), "the row must be gone");
    }
}
