//! Count handler — count documents matching filters.

use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, collection::filter_builder::FilterBuilder},
    },
    core::{CollectionDefinition, Registry, auth::SharedTokenProvider},
    db::{AccessResult, DbPool, LocaleContext},
    hooks::HookRunner,
    service::{
        CountDocumentsInput, RunnerReadHooks, ServiceContext, ServiceError, count_documents,
    },
};

/// Owned bundle for the `Count` spawn-blocking body.
struct CountBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    db_kind: String,
    collection: String,
    def: CollectionDefinition,
    token: Option<String>,
    where_json: Option<String>,
    locale_ctx: Option<LocaleContext>,
    search: Option<String>,
    include_drafts: bool,
}

/// Resolve auth, build the filter, and run `count_documents`.
fn count_blocking(input: CountBlockingInput) -> Result<i64, Status> {
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

    // Parse user filters only — the service layer injects `_status`
    // based on the `include_drafts` flag below, post-validation.
    let filters = FilterBuilder::new(&input.def.fields, &AccessResult::Allowed)
        .where_json(input.where_json.as_deref())
        .build()?;

    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
    let read_hooks = RunnerReadHooks::new(&input.runner, &conn);

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .pool(&input.pool)
        .conn(&conn)
        .read_hooks(&read_hooks)
        .user(user_doc)
        .build();

    let count_input = CountDocumentsInput::builder(&filters)
        .locale_ctx(input.locale_ctx.as_ref())
        .search(input.search.as_deref())
        .include_drafts(input.include_drafts)
        .build();

    count_documents(&ctx, &count_input).map_err(Status::from)
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Count documents matching filters (no per-document hooks).
    pub(in crate::api::handlers) async fn count_impl(
        &self,
        request: Request<content::CountRequest>,
    ) -> Result<Response<content::CountResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let input = CountBlockingInput {
            pool: self.pool.clone(),
            runner: self.hook_runner.clone(),
            token_provider: self.token_provider.clone(),
            registry: self.registry.clone(),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            def,
            token,
            where_json: req.r#where.clone(),
            locale_ctx,
            search: req.search.clone(),
            include_drafts: req.draft.unwrap_or(false),
        };

        let count = task::spawn_blocking(move || count_blocking(input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::CountResponse { count }))
    }
}
