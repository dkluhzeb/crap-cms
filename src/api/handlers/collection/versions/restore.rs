//! RestoreVersion handler — restore a document to a previous version.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, convert::document_to_proto},
    },
    service::{ServiceContext, restore_collection_version},
};

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

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let collection = req.collection.clone();
        let document_id = req.document_id.clone();
        let version_id = req.version_id.clone();
        let def_owned = def.clone();
        let locale_config = self.locale_config.clone();
        let event_transport = self.event_transport.clone();
        let cache = Some(self.cache.clone());

        let doc = task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool.get().map_err(|e| {
                error!("RestoreVersion pool error: {}", e);
                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;
            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());

            let ctx = ServiceContext::collection(&collection, &def_owned)
                .pool(&pool)
                .runner(&runner)
                .user(user_doc.as_ref())
                .event_transport(event_transport)
                .cache(cache)
                .build();

            let doc = restore_collection_version(&ctx, &document_id, &version_id, &locale_config)
                .map_err(Status::from)?;

            let proto_doc = document_to_proto(&doc, &collection);

            Ok(proto_doc)
        })
        .await
        .inspect_err(|e| error!("RestoreVersion task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::RestoreVersionResponse {
            document: Some(doc),
        }))
    }
}
