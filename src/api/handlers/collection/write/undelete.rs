//! Undelete handler — restore a soft-deleted document from trash.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::CollectionDefinition,
    service::{self, AppInfra, ServiceContext, ServiceError},
};

/// Owned bundle for the `Undelete` spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct UndeleteBlockingInput {
    infra: Arc<AppInfra>,
    headers: HashMap<String, String>,
    db_kind: String,
    def: CollectionDefinition,
    collection: String,
    id: String,
    token: Option<String>,
}

fn undelete_blocking(input: &UndeleteBlockingInput) -> Result<content::Document, Status> {
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
        .build();

    let doc = service::undelete_document(&ctx, &input.id)
        .map_err(|e| Status::from(e.reclassify(&input.db_kind)))?;

    Ok(document_to_proto(&doc, &input.collection))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Undelete a soft-deleted document from trash.
    pub(in crate::api::handlers) async fn undelete_impl(
        &self,
        request: Request<content::UndeleteRequest>,
    ) -> Result<Response<content::UndeleteResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Capability gate (soft-delete required) is enforced at the shared service
        // chokepoint `service::undelete_document`, so every surface agrees.

        let input = UndeleteBlockingInput {
            infra: Arc::clone(&self.infra),
            db_kind: self.db_kind.clone(),
            def,
            collection: req.collection.clone(),
            id: req.id.clone(),
            token,
            headers,
        };

        let proto_doc = task::spawn_blocking(move || undelete_blocking(&input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::UndeleteResponse {
            document: Some(proto_doc),
        }))
    }
}
