//! RestoreVersion handler — restore a document to a previous version.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        service::{
            ContentService, collection::helpers::strip_read_denied_proto_fields,
            convert::document_to_proto,
        },
    },
    core::event::EventOperation,
    db::{AccessResult, query},
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
        let def_fields = def.fields.clone();

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

            let version = query::find_version_by_id(&tx, &collection, &version_id)
                .map_err(|e| {
                    error!("RestoreVersion error: {}", e);
                    Status::internal("Internal error")
                })?
                .ok_or_else(|| Status::not_found(format!("Version '{}' not found", version_id)))?;

            let doc = query::restore_version(
                &tx,
                &collection,
                &def_owned,
                &document_id,
                &version.snapshot,
                &version.status,
                &locale_config,
            )
            .map_err(|e| {
                error!("RestoreVersion error: {}", e);
                Status::internal("Internal error")
            })?;

            tx.commit().map_err(|e| {
                error!("RestoreVersion commit error: {}", e);
                Status::internal("Internal error")
            })?;

            let mut proto_doc = document_to_proto(&doc, &collection);
            let user_doc_ref = auth_user.as_ref().map(|au| &au.user_doc);
            let mut conn = pool.get().map_err(|e| {
                error!("RestoreVersion field access pool error: {}", e);
                Status::internal("Internal error")
            })?;

            strip_read_denied_proto_fields(
                std::slice::from_mut(&mut proto_doc),
                &mut conn,
                &runner,
                &def_fields,
                user_doc_ref,
            );

            Ok((proto_doc, auth_user))
        })
        .await
        .map_err(|e| {
            error!("RestoreVersion task error: {}", e);
            Status::internal("Internal error")
        })??;

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
