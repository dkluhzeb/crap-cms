//! Bulk UpdateMany RPC handler.

use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use super::helpers::build_bulk_filters;

use crate::{
    api::{
        content,
        handlers::{ContentService, proto::prost_struct_to_json_map},
    },
    config::LocaleConfig,
    core::{
        CollectionDefinition, DocumentFields, Registry, SharedCache, SharedEventTransport,
        SharedTokenProvider,
    },
    db::{AccessResult, DbPool, LocaleContext},
    hooks::HookRunner,
    service::{self, ServiceContext, ServiceError, UpdateManyOptions},
};

/// Owned bundle for the `UpdateMany` spawn-blocking body.
struct UpdateManyBlockingInput {
    pool: DbPool,
    hook_runner: HookRunner,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    db_kind: String,
    collection: String,
    where_json: Option<String>,
    def: CollectionDefinition,
    locale_config: LocaleConfig,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
    token: Option<String>,
    data: DocumentFields,
    locale_ctx: Option<LocaleContext>,
    run_hooks: bool,
    draft: bool,
}

fn update_many_blocking(input: UpdateManyBlockingInput) -> Result<i64, Status> {
    let mut conn = input
        .pool
        .get()
        .map_err(|e| Status::from(ServiceError::classify(e, &input.db_kind)))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token,
        &*input.token_provider,
        &input.registry,
        &conn,
    )?;

    let read_access = ContentService::check_access_blocking(
        input.def.access.read.as_deref(),
        &auth_user,
        None,
        None,
        &input.hook_runner,
        &mut conn,
    )?;

    if matches!(read_access, AccessResult::Denied) {
        return Err(Status::permission_denied("Read access denied"));
    }

    drop(conn);

    let filters = build_bulk_filters(
        &input.collection,
        &input.def,
        &read_access,
        input.where_json.as_deref(),
        !input.draft,
    )?;

    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .pool(&input.pool)
        .runner(&input.hook_runner)
        .user(user_doc)
        .event_transport(input.event_transport)
        .cache(input.cache)
        .build();

    let update_opts = UpdateManyOptions {
        locale_ctx: input.locale_ctx.as_ref(),
        run_hooks: input.run_hooks,
        draft: input.draft,
        ui_locale: None,
    };

    let result = service::update_many(
        &ctx,
        filters,
        input.data,
        &input.locale_config,
        &update_opts,
    )
    .map_err(|e| Status::from(e.reclassify(&input.db_kind)))?;

    Ok(result.modified)
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Bulk update matching documents. Runs per-document lifecycle hooks by default.
    pub(in crate::api::handlers) async fn update_many_impl(
        &self,
        request: Request<content::UpdateManyRequest>,
    ) -> Result<Response<content::UpdateManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let mut data: DocumentFields = req
            .data
            .map(|s| prost_struct_to_json_map(&s))
            .unwrap_or_default()
            .into();

        if def.is_auth_collection() && data.contains_key("password") {
            return Err(Status::invalid_argument(
                "Password updates are not supported in UpdateMany. Use Update for individual documents.",
            ));
        }

        if def.is_auth_collection() {
            data.remove("password");
        }

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let input = UpdateManyBlockingInput {
            pool: self.pool.clone(),
            hook_runner: self.hook_runner.clone(),
            token_provider: self.token_provider.clone(),
            registry: self.registry.clone(),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            where_json: req.r#where.clone(),
            def,
            locale_config: self.locale_config.clone(),
            event_transport: self.event_transport.clone(),
            cache: Some(self.cache.clone()),
            token,
            data,
            locale_ctx,
            run_hooks: req.hooks.unwrap_or(true),
            draft: req.draft.unwrap_or(false),
        };

        let modified = task::spawn_blocking(move || update_many_blocking(input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::UpdateManyResponse { modified }))
    }
}
