//! RestoreVersion handler — restore a document to a previous version.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        service::{ContentService, convert::document_to_proto},
    },
    core::event::EventOperation,
    db::AccessResult,
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Restore a document to a previous version.
    pub(in crate::api::service) async fn restore_version_impl(
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
        let access_update = def.access.update.clone();
        let def_owned = def.clone();
        let locale_config = self.locale_config.clone();

        let (doc, auth_user) = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| {
                error!("RestoreVersion pool error: {}", e);
                Status::internal("Internal error")
            })?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let access_result = ContentService::check_access_blocking(
                access_update.as_deref(),
                &auth_user,
                Some(&document_id),
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Update access denied"));
            }

            let tx = conn.transaction_immediate().map_err(|e| {
                error!("RestoreVersion tx error: {}", e);
                Status::internal("Internal error")
            })?;

            let doc = crate::service::version_ops::restore_collection_version(
                &tx,
                &collection,
                &def_owned,
                &document_id,
                &version_id,
                &locale_config,
            )
            .map_err(Status::from)?;

            tx.commit().map_err(|e| {
                error!("RestoreVersion commit error: {}", e);
                Status::internal("Internal error")
            })?;

            let proto_doc = document_to_proto(&doc, &collection);

            Ok((proto_doc, auth_user))
        })
        .await
        .inspect_err(|e| error!("RestoreVersion task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        self.publish_mutation_event(
            &req.collection,
            &req.document_id,
            EventOperation::Update,
            &auth_user,
        );

        Ok(Response::new(content::RestoreVersionResponse {
            document: Some(doc),
        }))
    }
}
