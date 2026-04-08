//! Validate handler — check field data against collection rules without persisting.

use std::collections::HashMap;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            convert::{prost_struct_to_hashmap, prost_struct_to_json_map},
        },
    },
    db::LocaleContext,
    service::{self, RunnerWriteHooks, ServiceError, ValidateContext, WriteInput},
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Validate document data without persisting — returns per-field errors.
    pub(in crate::api::handlers) async fn validate_impl(
        &self,
        request: Request<content::ValidateRequest>,
    ) -> Result<Response<content::ValidateResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();

        let data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let operation = if req.id.is_some() { "update" } else { "create" };

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let def_owned = def;

        let result = task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());

            let write_hooks = RunnerWriteHooks::new(&runner);

            let ctx = ValidateContext {
                slug: &collection,
                table_name: &collection,
                fields: &def_owned.fields,
                hooks: &def_owned.hooks,
                operation,
                exclude_id: req.id.as_deref(),
                soft_delete: def_owned.soft_delete,
            };

            let input = WriteInput::builder(data, &join_data)
                .locale_ctx(locale_ctx.as_ref())
                .draft(req.draft.unwrap_or(false))
                .build();

            match service::validate_document(&conn, &write_hooks, &ctx, input, user_doc.as_ref()) {
                Ok(()) => Ok(content::ValidateResponse {
                    valid: true,
                    errors: HashMap::new(),
                }),
                Err(ServiceError::Validation(ve)) => Ok(content::ValidateResponse {
                    valid: false,
                    errors: ve.to_field_map(),
                }),
                Err(e) => Err(Status::from(e.reclassify(&db_kind))),
            }
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(result))
    }
}
