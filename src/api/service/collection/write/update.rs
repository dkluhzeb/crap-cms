//! Update handler — update an existing document by ID.

use anyhow::Context as _;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        service::{
            ContentService,
            collection::helpers::{
                extract_auth_password, map_db_error, strip_read_denied_proto_fields,
            },
            convert::{document_to_proto, prost_struct_to_hashmap, prost_struct_to_json_map},
        },
    },
    core::event::EventOperation,
    db::{AccessResult, LocaleContext},
    service::{self, WriteInput},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Update an existing document by ID, running before/after hooks within a transaction.
    pub(in crate::api::service) async fn update_impl(
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

        let mut join_data = req
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
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let def_fields = def.fields.clone();
        let def_owned = def;

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

            {
                let tx = conn
                    .transaction()
                    .context("Transaction for field access")
                    .map_err(|e| {
                        error!("Field access tx error: {}", e);
                        Status::internal("Internal error")
                    })?;

                let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
                let denied =
                    runner.check_field_write_access(&def_owned.fields, user_doc, "update", &tx);

                tx.commit()
                    .context("Commit field access transaction")
                    .map_err(|e| {
                        error!("Field access commit error: {}", e);
                        Status::internal("Internal error")
                    })?;

                for name in &denied {
                    data.remove(name);
                    join_data.remove(name);
                }
            }

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
            let auth_user_ui_locale = auth_user.as_ref().map(|au| au.ui_locale.clone());
            let ui_locale = user_doc.as_ref().and_then(|_| auth_user_ui_locale.clone());

            let (doc, _req_context) = service::update_document_with_conn(
                &mut conn,
                &runner,
                &collection,
                &id,
                &def_owned,
                WriteInput::builder(data, &join_data)
                    .password(password.as_deref())
                    .locale_ctx(locale_ctx.as_ref())
                    .draft(req.draft.unwrap_or(false))
                    .ui_locale(ui_locale)
                    .build(),
                user_doc.as_ref(),
            )
            .map_err(|e| map_db_error(e, "Update error", &db_kind))?;

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

        Ok(Response::new(content::UpdateResponse {
            document: Some(proto_doc),
        }))
    }
}
