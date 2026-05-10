//! FindByID handler — fetch a single document by ID.

use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, convert::document_to_proto},
    },
    core::{CollectionDefinition, Registry, SharedCache, SharedTokenProvider},
    db::{DbPool, LocaleContext, query::SharedPopulateSingleflight},
    hooks::HookRunner,
    service::{FindByIdInput, RunnerReadHooks, ServiceContext, ServiceError, find_document_by_id},
};

/// Owned bundle for the `FindById` spawn-blocking body.
struct FindByIdBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    db_kind: String,
    collection: String,
    id: String,
    pop_cache: SharedCache,
    singleflight: SharedPopulateSingleflight,
    def: CollectionDefinition,
    token: Option<String>,
    depth: i32,
    select: Option<Vec<String>>,
    locale_ctx: Option<LocaleContext>,
    use_draft_version: bool,
    include_deleted: bool,
}

fn find_by_id_blocking(input: FindByIdBlockingInput) -> Result<Option<content::Document>, Status> {
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

    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
    let read_hooks = RunnerReadHooks::new(&input.runner, &conn);

    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .pool(&input.pool)
        .conn(&conn)
        .read_hooks(&read_hooks)
        .user(user_doc)
        .build();

    let find_input = FindByIdInput::builder(&input.id)
        .depth(input.depth)
        .select(input.select.as_deref())
        .locale_ctx(input.locale_ctx.as_ref())
        .registry(Some(&input.registry))
        .use_draft(input.use_draft_version)
        .cache(Some(&*input.pop_cache))
        .include_deleted(input.include_deleted)
        .singleflight(Some(input.singleflight))
        .build();

    let doc = find_document_by_id(&ctx, &find_input).map_err(Status::from)?;

    match doc {
        Some(d) => Ok(Some(document_to_proto(&d, &input.collection))),
        None => Err(Status::not_found(format!(
            "Document '{}' not found in '{}'",
            input.id, input.collection
        ))),
    }
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Find a single document by ID with optional relationship population depth.
    pub(in crate::api::handlers) async fn find_by_id_impl(
        &self,
        request: Request<content::FindByIdRequest>,
    ) -> Result<Response<content::FindByIdResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let depth = req
            .depth
            .unwrap_or(self.default_depth)
            .max(0)
            .min(self.max_depth);

        let select = if req.select.is_empty() {
            None
        } else {
            Some(req.select.clone())
        };

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let use_draft_version =
            req.draft.unwrap_or(false) && def.has_drafts() && def.has_versions();

        let include_deleted = req.trash.unwrap_or(false) && def.soft_delete;

        let input = FindByIdBlockingInput {
            pool: self.pool.clone(),
            runner: self.hook_runner.clone(),
            token_provider: self.token_provider.clone(),
            registry: self.registry.clone(),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            id: req.id.clone(),
            pop_cache: self.cache.clone(),
            singleflight: self.populate_singleflight.clone(),
            def,
            token,
            depth,
            select,
            locale_ctx,
            use_draft_version,
            include_deleted,
        };

        let result = task::spawn_blocking(move || find_by_id_blocking(input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::FindByIdResponse {
            document: result,
        }))
    }
}
