//! Delete handler — delete a document by ID (soft or hard).

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        service::{ContentService, collection::helpers::map_db_error},
    },
    core::event::EventOperation,
    service,
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Delete a document by ID, running before/after delete hooks.
    ///
    /// Permission check depends on the type of deletion:
    /// - Soft delete (trash): check `access.trash`, falling back to `access.update`
    /// - Permanent delete (`force_hard_delete` or no `soft_delete`): check `access.delete`
    pub(in crate::api::service) async fn delete_impl(
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

        let auth_user = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());

            service::delete_document_with_conn(
                &mut conn,
                &runner,
                &collection,
                &id,
                &def_clone,
                user_doc.as_ref(),
                Some(&*storage),
                Some(&locale_config),
            )
            .map_err(|e| Status::from(e.reclassify(&db_kind)))?;

            Ok(auth_user)
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        self.publish_mutation_event(&req.collection, &req.id, EventOperation::Delete, &auth_user);

        Ok(Response::new(content::DeleteResponse {
            success: true,
            soft_deleted: will_soft_delete,
        }))
    }
}
