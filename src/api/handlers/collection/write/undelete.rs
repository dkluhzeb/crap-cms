//! Undelete handler — restore a soft-deleted document from trash.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, convert::document_to_proto},
    },
    core::event::EventOperation,
    service::{self, ServiceError},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Undelete a soft-deleted document from trash.
    pub(in crate::api::handlers) async fn undelete_impl(
        &self,
        request: Request<content::UndeleteRequest>,
    ) -> Result<Response<content::UndeleteResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.soft_delete {
            return Err(Status::failed_precondition(
                "Collection does not have soft_delete enabled",
            ));
        }

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let def_clone = def.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let (proto_doc, auth_user) = task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
            drop(conn);

            let doc = service::undelete_document(
                &pool,
                &runner,
                &collection,
                &id,
                &def_clone,
                user_doc.as_ref(),
                false,
            )
            .map_err(|e| Status::from(e.reclassify(&db_kind)))?;

            let proto_doc = document_to_proto(&doc, &collection);

            Ok((proto_doc, auth_user))
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        self.publish_mutation_event(&req.collection, &req.id, EventOperation::Update, &auth_user);

        Ok(Response::new(content::UndeleteResponse {
            document: Some(proto_doc),
        }))
    }
}
