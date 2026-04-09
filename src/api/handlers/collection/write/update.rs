//! Update handler — update an existing document by ID.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            collection::helpers::extract_auth_password,
            convert::{document_to_proto, prost_struct_to_hashmap, prost_struct_to_json_map},
        },
    },
    core::event::EventOperation,
    db::LocaleContext,
    service::{self, ServiceError, WriteInput},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Update an existing document by ID, running before/after hooks within a transaction.
    pub(in crate::api::handlers) async fn update_impl(
        &self,
        request: Request<content::UpdateRequest>,
    ) -> Result<Response<content::UpdateResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if req.unpublish.unwrap_or(false) && def.has_versions() {
            return self.unpublish_impl(token, &req, &def).await;
        }

        let join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();

        let mut data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        let password = extract_auth_password(
            &mut data,
            def.is_auth_collection(),
            &self.password_policy,
            true,
        )?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let def_owned = def;

        let (proto_doc, auth_user) = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            // Field write access is now checked inside service::update_document_core
            // via WriteHooks::field_write_denied (using the transaction connection).

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
            let auth_user_ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
            let ui_locale = user_doc.as_ref().and_then(|_| auth_user_ui_locale.clone());
            let input = WriteInput::builder(data, &join_data)
                .password(password.as_deref())
                .locale_ctx(locale_ctx.as_ref())
                .draft(req.draft.unwrap_or(false))
                .ui_locale(ui_locale)
                .build();

            let (doc, _req_context) = service::update_document_with_conn(
                &mut conn,
                &runner,
                &collection,
                &id,
                &def_owned,
                input,
                user_doc.as_ref(),
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
