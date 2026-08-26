//! Unpublish handler — revert a published document to draft status.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task;
use tonic::{Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::CollectionDefinition,
    service::{self, AppInfra, ServiceContext, ServiceError},
};

/// Owned bundle for the `Unpublish` spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct UnpublishBlockingInput {
    infra: Arc<AppInfra>,
    headers: HashMap<String, String>,
    db_kind: String,
    collection: String,
    id: String,
    def: CollectionDefinition,
    token: Option<String>,
    events: bool,
}

fn unpublish_blocking(input: &UnpublishBlockingInput) -> Result<content::Document, Status> {
    let infra = &input.infra;

    let conn = infra
        .pool
        .get()
        .map_err(|e| Status::from(ServiceError::classify(e, &input.db_kind)))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*infra.token_provider,
        &infra.hook_runner,
        &infra.registry,
        &conn,
    )?;

    let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());

    drop(conn);

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .infra(infra)
        .user(user_doc.as_ref())
        .emit_events(input.events)
        .build();

    let doc = service::unpublish_document(&ctx, &input.id)
        .map_err(|e| Status::from(e.reclassify(&input.db_kind)))?;

    Ok(document_to_proto(&doc, &input.collection))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Unpublish a document: set status to draft, create version snapshot.
    pub(in crate::api::handlers) async fn unpublish_impl(
        &self,
        token: Option<String>,
        headers: HashMap<String, String>,
        req: &content::UpdateRequest,
        def: &CollectionDefinition,
    ) -> Result<Response<content::UpdateResponse>, Status> {
        let input = UnpublishBlockingInput {
            infra: Arc::clone(&self.infra),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            id: req.id.clone(),
            def: def.clone(),
            token,
            headers,
            events: req.events.unwrap_or(true),
        };

        let proto_doc = task::spawn_blocking(move || unpublish_blocking(&input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::UpdateResponse {
            document: Some(proto_doc),
        }))
    }
}
