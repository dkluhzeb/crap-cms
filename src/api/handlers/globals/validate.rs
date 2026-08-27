//! `ValidateGlobal` handler — check global field data against its rules
//! without persisting. Mirrors `Validate` (collections) but targets the
//! `_global_<slug>` table and always runs in update mode against the
//! singleton `default` row.

use std::{collections::HashMap, sync::Arc};

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::data_map_to_json_map},
    },
    core::{DocumentFields, GlobalDefinition, Registry, SharedTokenProvider},
    db::{DbPool, LocaleContext, query::helpers::global_table},
    hooks::HookRunner,
    service::{self, RunnerWriteHooks, ServiceError, ValidateContext, WriteInput},
};

/// Owned bundle for the `ValidateGlobal` spawn-blocking body.
struct ValidateGlobalBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    headers: HashMap<String, String>,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    db_kind: String,
    slug: String,
    def: GlobalDefinition,
    token: Option<String>,
    data: DocumentFields,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
}

fn validate_global_blocking(
    input: ValidateGlobalBlockingInput,
) -> Result<content::ValidateResponse, Status> {
    let conn = input
        .pool
        .get()
        .map_err(|e| Status::from(ServiceError::classify(e, &input.db_kind)))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*input.token_provider,
        &input.runner,
        &input.registry,
        &conn,
    )?;

    let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());

    // `.with_conn` so field-level write-access stripping actually runs during the
    // dry-run (parity with the collection validate handler) — otherwise a
    // write-denied field would be validated even though a real write would strip it.
    let write_hooks = RunnerWriteHooks::new(&input.runner).with_conn(&conn);

    // Globals are keyed by the fixed `default` row in `_global_<slug>` —
    // validate as an update that excludes it from unique checks.
    let table = global_table(&input.slug);
    let ctx = ValidateContext {
        slug: &input.slug,
        table_name: &table,
        fields: &input.def.fields,
        hooks: &input.def.hooks,
        operation: "update",
        exclude_id: Some("default"),
        soft_delete: false,
        supports_drafts: input.def.has_drafts(),
        // Globals have no collection-level `required_locales` default.
        required_locales: None,
    };

    let write_input = WriteInput::builder(input.data)
        .locale_ctx(input.locale_ctx.as_ref())
        .draft(input.draft)
        .build();

    let result =
        service::validate_document(&conn, &write_hooks, &ctx, write_input, user_doc.as_ref());
    match service::validate_outcome(result) {
        Ok((valid, errors)) => Ok(content::ValidateResponse { valid, errors }),
        Err(e) => Err(Status::from(e.reclassify(&input.db_kind))),
    }
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Validate global document data without persisting — returns per-field errors.
    pub(in crate::api::handlers) async fn validate_global_impl(
        &self,
        request: Request<content::ValidateGlobalRequest>,
    ) -> Result<Response<content::ValidateResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_global_def(&req.slug)?;

        let data: DocumentFields = req
            .data
            .map(|s| data_map_to_json_map(&s))
            .unwrap_or_default()
            .into();

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.infra.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let input = ValidateGlobalBlockingInput {
            pool: self.infra.pool.clone(),
            runner: self.infra.hook_runner.clone(),
            token_provider: self.infra.token_provider.clone(),
            registry: Arc::clone(&self.infra.registry),
            db_kind: self.db_kind.clone(),
            slug: req.slug.clone(),
            def,
            token,
            headers,
            data,
            locale_ctx,
            draft: req.draft.unwrap_or(false),
        };

        let result = task::spawn_blocking(move || validate_global_blocking(input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(result))
    }
}
