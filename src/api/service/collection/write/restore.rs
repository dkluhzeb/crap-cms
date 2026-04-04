//! Restore handler — restore a soft-deleted document from trash.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        service::{
            ContentService,
            collection::helpers::{map_db_error, strip_read_denied_proto_fields},
            convert::document_to_proto,
        },
    },
    core::event::EventOperation,
    db::AccessResult,
    service,
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Restore a soft-deleted document from trash.
    pub(in crate::api::service) async fn restore_impl(
        &self,
        request: Request<content::RestoreRequest>,
    ) -> Result<Response<content::RestoreResponse>, Status> {
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
        let def_fields = def.fields.clone();
        let trash_access = def.access.resolve_trash().map(|s| s.to_string());

        let (proto_doc, auth_user) = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let access_result = ContentService::check_access_blocking(
                trash_access.as_deref(),
                &auth_user,
                Some(&id),
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Restore access denied"));
            }

            drop(conn);

            let doc = service::restore_document(&pool, &collection, &id, &def_clone)
                .map_err(|e| map_db_error(e, "Restore error", &db_kind))?;

            let mut proto_doc = document_to_proto(&doc, &collection);
            let user_doc_ref = auth_user.as_ref().map(|au| &au.user_doc);
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

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
            error!("Task error: {}", e);
            Status::internal("Internal error")
        })??;

        self.publish_mutation_event(&req.collection, &req.id, EventOperation::Update, &auth_user);

        Ok(Response::new(content::RestoreResponse {
            document: Some(proto_doc),
        }))
    }
}
