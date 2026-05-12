//! Delete handler — delete a document by ID (soft or hard).

use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    config::LocaleConfig,
    core::{
        CollectionDefinition, Registry, SharedCache, SharedEventTransport,
        SharedInvalidationTransport, SharedStorage, SharedTokenProvider,
    },
    db::DbPool,
    hooks::HookRunner,
    service::{self, ServiceError},
};

/// Owned bundle for the `Delete` spawn-blocking body.
struct DeleteBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    db_kind: String,
    def: CollectionDefinition,
    collection: String,
    id: String,
    storage: SharedStorage,
    locale_config: LocaleConfig,
    invalidation_transport: SharedInvalidationTransport,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
    token: Option<String>,
}

fn delete_blocking(input: DeleteBlockingInput) -> Result<(), Status> {
    let conn = input
        .pool
        .get()
        .map_err(|e| Status::from(ServiceError::classify(e, &input.db_kind)))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token,
        &*input.token_provider,
        &input.registry,
        &conn,
    )?;

    let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());

    // Service-layer delete publishes the invalidation signal on
    // hard-delete of auth collections when a transport is attached.
    let ctx = service::ServiceContext::collection(&input.collection, &input.def)
        .pool(&input.pool)
        .runner(&input.runner)
        .user(user_doc.as_ref())
        .invalidation_transport(Some(input.invalidation_transport))
        .event_transport(input.event_transport)
        .cache(input.cache)
        .build();

    service::delete_document(
        &ctx,
        &input.id,
        Some(&*input.storage),
        Some(&input.locale_config),
    )
    .map_err(|e| Status::from(e.reclassify(&input.db_kind)))?;

    Ok(())
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Delete a document by ID, running before/after delete hooks.
    ///
    /// Permission check depends on the type of deletion:
    /// - Soft delete (trash): check `access.trash`, falling back to `access.update`
    /// - Permanent delete (`force_hard_delete` or no `soft_delete`): check `access.delete`
    pub(in crate::api::handlers) async fn delete_impl(
        &self,
        request: Request<content::DeleteRequest>,
    ) -> Result<Response<content::DeleteResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let mut def = self.get_collection_def(&req.collection)?;

        let will_soft_delete = def.soft_delete && !req.force_hard_delete;

        if req.force_hard_delete && def.soft_delete {
            def.soft_delete = false;
        }

        let input = DeleteBlockingInput {
            pool: self.pool.clone(),
            runner: self.hook_runner.clone(),
            token_provider: self.token_provider.clone(),
            registry: Arc::clone(&self.registry),
            db_kind: self.db_kind.clone(),
            def,
            collection: req.collection.clone(),
            id: req.id.clone(),
            storage: self.storage.clone(),
            locale_config: self.locale_config.clone(),
            invalidation_transport: self.invalidation_transport.clone(),
            event_transport: self.event_transport.clone(),
            cache: Some(self.cache.clone()),
            token,
        };

        task::spawn_blocking(move || delete_blocking(input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::DeleteResponse {
            success: true,
            soft_deleted: will_soft_delete,
        }))
    }
}
