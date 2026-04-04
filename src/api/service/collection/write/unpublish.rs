//! Unpublish handler — revert a published document to draft status.

use std::slice;

use tokio::task;
use tonic::{Response, Status};
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
    core::{CollectionDefinition, event::EventOperation},
    db::AccessResult,
    service,
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Unpublish a document: set status to draft, create version snapshot.
    pub(in crate::api::service) async fn unpublish_impl(
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
        let def_fields = def.fields.clone();
        let def_owned = def.clone();

        let (proto_doc, auth_user) = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let access_result = ContentService::check_access_blocking(
                def_owned.access.update.as_deref(),
                &auth_user,
                Some(&id),
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Update access denied"));
            }

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
            drop(conn);

            let doc = service::unpublish_document(
                &pool,
                &runner,
                &collection,
                &id,
                &def_owned,
                user_doc.as_ref(),
            )
            .map_err(|e| map_db_error(e, "Unpublish error", &db_kind))?;

            let mut proto_doc = document_to_proto(&doc, &collection);
            let user_doc_ref = auth_user.as_ref().map(|au| &au.user_doc);
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            strip_read_denied_proto_fields(
                slice::from_mut(&mut proto_doc),
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

        Ok(Response::new(content::UpdateResponse {
            document: Some(proto_doc),
        }))
    }
}
