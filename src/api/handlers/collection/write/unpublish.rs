//! Unpublish handler — revert a published document to draft status.

use tokio::task;
use tonic::{Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, convert::document_to_proto},
    },
    core::{CollectionDefinition, event::EventOperation},
    service::{self, ServiceError},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Unpublish a document: set status to draft, create version snapshot.
    pub(in crate::api::handlers) async fn unpublish_impl(
        &self,
        token: Option<String>,
        req: &content::UpdateRequest,
        def: &CollectionDefinition,
    ) -> Result<Response<content::UpdateResponse>, Status> {
        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let def_owned = def.clone();

        let (proto_doc, auth_user) = task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());

            drop(conn);

            let doc = service::unpublish_document(
                &pool,
                &runner,
                &collection,
                &id,
                &def_owned,
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

        Ok(Response::new(content::UpdateResponse {
            document: Some(proto_doc),
        }))
    }
}
