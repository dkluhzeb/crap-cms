//! Delete handler — delete a document by ID (soft or hard).

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    service::{self, ServiceError},
};

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

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let def_clone = def.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let storage = self.storage.clone();
        let locale_config = self.locale_config.clone();
        let invalidation_transport = self.invalidation_transport.clone();
        let event_transport = self.event_transport.clone();

        task::spawn_blocking(move || -> Result<(), Status> {
            let conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());

            // Service-layer delete publishes the invalidation signal on
            // hard-delete of auth collections when a transport is attached.
            let ctx = service::ServiceContext::collection(&collection, &def_clone)
                .pool(&pool)
                .runner(&runner)
                .user(user_doc.as_ref())
                .invalidation_transport(Some(invalidation_transport))
                .event_transport(event_transport)
                .build();
            service::delete_document(&ctx, &id, Some(&*storage), Some(&locale_config))
                .map_err(|e| Status::from(e.reclassify(&db_kind)))?;

            Ok(())
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        self.on_collection_mutation();

        Ok(Response::new(content::DeleteResponse {
            success: true,
            soft_deleted: will_soft_delete,
        }))
    }
}
