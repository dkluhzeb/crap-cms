//! Validate handler — check field data against collection rules without persisting.

use std::{collections::HashMap, sync::Arc};

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, convert::prost_struct_to_json_map},
    },
    core::{CollectionDefinition, DocumentFields, Registry, auth::SharedTokenProvider},
    db::{DbPool, LocaleContext},
    hooks::HookRunner,
    service::{self, RunnerWriteHooks, ServiceError, ValidateContext, WriteInput},
};

/// Owned bundle for the `Validate` spawn-blocking body.
struct ValidateBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    db_kind: String,
    collection: String,
    def: CollectionDefinition,
    token: Option<String>,
    data: DocumentFields,
    locale_ctx: Option<LocaleContext>,
    operation: &'static str,
    exclude_id: Option<String>,
    draft: bool,
}

fn validate_blocking(input: ValidateBlockingInput) -> Result<content::ValidateResponse, Status> {
    let conn = input
        .pool
        .get()
        .map_err(|e| Status::from(ServiceError::classify(e, &input.db_kind)))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token,
        &*input.token_provider,
        &input.registry,
        &conn,
    )?;

    let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());

    let write_hooks = RunnerWriteHooks::new(&input.runner);

    let ctx = ValidateContext {
        slug: &input.collection,
        table_name: &input.collection,
        fields: &input.def.fields,
        hooks: &input.def.hooks,
        operation: input.operation,
        exclude_id: input.exclude_id.as_deref(),
        soft_delete: input.def.soft_delete,
    };

    let write_input = WriteInput::builder(input.data)
        .locale_ctx(input.locale_ctx.as_ref())
        .draft(input.draft)
        .build();

    match service::validate_document(&conn, &write_hooks, &ctx, write_input, user_doc.as_ref()) {
        Ok(()) => Ok(content::ValidateResponse {
            valid: true,
            errors: HashMap::new(),
        }),
        Err(ServiceError::Validation(ve)) => Ok(content::ValidateResponse {
            valid: false,
            errors: ve.to_field_map(),
        }),
        Err(e) => Err(Status::from(e.reclassify(&input.db_kind))),
    }
}

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

        let data: DocumentFields = req
            .data
            .map(|s| prost_struct_to_json_map(&s))
            .unwrap_or_default()
            .into();

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let input = ValidateBlockingInput {
            pool: self.pool.clone(),
            runner: self.hook_runner.clone(),
            token_provider: self.token_provider.clone(),
            registry: self.registry.clone(),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            def,
            token,
            data,
            locale_ctx,
            operation: if req.id.is_some() { "update" } else { "create" },
            exclude_id: req.id.clone(),
            draft: req.draft.unwrap_or(false),
        };

        let result = task::spawn_blocking(move || validate_blocking(input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(result))
    }
}
