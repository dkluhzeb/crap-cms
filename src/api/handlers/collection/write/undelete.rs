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
    core::{
        CollectionDefinition, Registry, SharedCache, SharedEventTransport, SharedTokenProvider,
    },
    db::DbPool,
    hooks::HookRunner,
    service::{self, ServiceContext, ServiceError},
};

/// Owned bundle for the `Undelete` spawn-blocking body.
struct UndeleteBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    headers: HashMap<String, String>,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    db_kind: String,
    def: CollectionDefinition,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
    collection: String,
    id: String,
    token: Option<String>,
}

fn undelete_blocking(input: UndeleteBlockingInput) -> Result<content::Document, Status> {
    let conn = input
        .pool
        .get()
        .map_err(|e| Status::from(ServiceError::classify(e, &input.db_kind)))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*input.token_provider,
        &input.runner,
        &input.registry,
        &conn,
    )?;

    let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
    drop(conn);

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .pool(&input.pool)
        .runner(&input.runner)
        .user(user_doc.as_ref())
        .event_transport(input.event_transport)
        .cache(input.cache)
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

        if !def.soft_delete {
            return Err(Status::failed_precondition(
                "Collection does not have soft_delete enabled",
            ));
        }

        let input = UndeleteBlockingInput {
            pool: self.pool.clone(),
            runner: self.hook_runner.clone(),
            token_provider: self.token_provider.clone(),
            registry: Arc::clone(&self.registry),
            db_kind: self.db_kind.clone(),
            def,
            event_transport: self.event_transport.clone(),
            cache: Some(self.cache.clone()),
            collection: req.collection.clone(),
            id: req.id.clone(),
            token,
            headers,
        };

        let proto_doc = task::spawn_blocking(move || undelete_blocking(input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::UndeleteResponse {
            document: Some(proto_doc),
        }))
    }
}
