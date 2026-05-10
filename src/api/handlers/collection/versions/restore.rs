//! RestoreVersion handler — restore a document to a previous version.

use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, convert::document_to_proto},
    },
    config::LocaleConfig,
    core::{
        CollectionDefinition, Registry, SharedCache, SharedEventTransport, SharedTokenProvider,
    },
    db::DbPool,
    hooks::HookRunner,
    service::{ServiceContext, restore_collection_version},
};

/// Owned bundle for the `RestoreVersion` spawn-blocking body.
struct RestoreVersionBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    collection: String,
    document_id: String,
    version_id: String,
    def: CollectionDefinition,
    locale_config: LocaleConfig,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
    token: Option<String>,
}

fn restore_version_blocking(
    input: RestoreVersionBlockingInput,
) -> Result<content::Document, Status> {
    let conn = input
        .pool
        .get()
        .inspect_err(|e| error!("RestoreVersion pool error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token,
        &*input.token_provider,
        &input.registry,
        &conn,
    )?;
    let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .pool(&input.pool)
        .runner(&input.runner)
        .user(user_doc.as_ref())
        .event_transport(input.event_transport)
        .cache(input.cache)
        .build();

    let doc = restore_collection_version(
        &ctx,
        &input.document_id,
        &input.version_id,
        &input.locale_config,
    )
    .map_err(Status::from)?;

    Ok(document_to_proto(&doc, &input.collection))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Restore a document to a previous version.
    pub(in crate::api::handlers) async fn restore_version_impl(
        &self,
        request: Request<content::RestoreVersionRequest>,
    ) -> Result<Response<content::RestoreVersionResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.has_versions() {
            return Err(Status::failed_precondition(format!(
                "Collection '{}' does not have versioning enabled",
                req.collection
            )));
        }

        let input = RestoreVersionBlockingInput {
            pool: self.pool.clone(),
            runner: self.hook_runner.clone(),
            token_provider: self.token_provider.clone(),
            registry: self.registry.clone(),
            collection: req.collection.clone(),
            document_id: req.document_id.clone(),
            version_id: req.version_id.clone(),
            def,
            locale_config: self.locale_config.clone(),
            event_transport: self.event_transport.clone(),
            cache: Some(self.cache.clone()),
            token,
        };

        let doc = task::spawn_blocking(move || restore_version_blocking(input))
            .await
            .inspect_err(|e| error!("RestoreVersion task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::RestoreVersionResponse {
            document: Some(doc),
        }))
    }
}
