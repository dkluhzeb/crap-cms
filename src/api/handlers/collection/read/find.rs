//! Find handler — query documents with filters, sorting, and pagination.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{
            ContentService, collection::filter_builder::FilterBuilder, proto::document_to_proto,
        },
    },
    core::{CollectionDefinition, Registry, SharedCache, SharedTokenProvider},
    db::{AccessResult, DbPool, FindQuery, LocaleContext, SharedPopulateSingleflight, query},
    hooks::HookRunner,
    service::{FindDocumentsInput, RunnerReadHooks, ServiceContext, ServiceError, find_documents},
};

use crate::api::handlers::proto::pagination_result_to_proto;

/// Owned bundle for the `Find` spawn-blocking body.
struct FindBlockingInput {
    pool: DbPool,
    runner: HookRunner,
    headers: HashMap<String, String>,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    db_kind: String,
    collection: String,
    def: CollectionDefinition,
    pop_cache: SharedCache,
    singleflight: SharedPopulateSingleflight,
    token: Option<String>,
    find_query: FindQuery,
    locale_ctx: Option<LocaleContext>,
    select: Option<Vec<String>>,
    depth: i32,
    cursor_enabled: bool,
    is_trash: bool,
    include_drafts: bool,
}

fn find_blocking(
    input: FindBlockingInput,
) -> Result<(Vec<content::Document>, content::PaginationInfo), Status> {
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

    query::validate_query_fields(&input.def, &input.find_query, input.locale_ctx.as_ref())
        .map_err(|e| Status::invalid_argument(e.to_string()))?;

    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

    let read_hooks = RunnerReadHooks::new(&input.runner, &conn, user_doc, None);
    let ctx = ServiceContext::collection(&input.collection, &input.def)
        .pool(&input.pool)
        .conn(&conn)
        .read_hooks(&read_hooks)
        .user(user_doc)
        .build();

    let find_input = FindDocumentsInput::builder(&input.find_query)
        .depth(input.depth)
        .select(input.select.as_deref())
        .locale_ctx(input.locale_ctx.as_ref())
        .registry(Some(&input.registry))
        .cache(Some(&*input.pop_cache))
        .cursor_enabled(input.cursor_enabled)
        .trash(input.is_trash)
        .include_drafts(input.include_drafts)
        .singleflight(Some(input.singleflight))
        .build();

    let result = find_documents(&ctx, &find_input).map_err(Status::from)?;

    let proto_docs: Vec<_> = result
        .docs
        .iter()
        .map(|doc| document_to_proto(doc, &input.collection))
        .collect();

    Ok((proto_docs, pagination_result_to_proto(&result.pagination)))
}

/// Build a `FindQuery` from the gRPC request parameters.
///
/// Produces a *user* query — system filters (`_status`, `_deleted_at`) are
/// injected by the service layer based on the typed `trash` / `include_drafts`
/// flags on `FindDocumentsInput`. The handler still steers presentation order
/// to `-_deleted_at` for trash listings since the default sort order is a
/// presentation concern, not a service-layer semantic.
fn build_find_query(
    req: &content::FindRequest,
    def: &crate::core::CollectionDefinition,
    pagination: &query::FindPagination,
    select: Option<&[String]>,
) -> Result<FindQuery, Status> {
    let filters = FilterBuilder::new(&def.fields, &AccessResult::Allowed)
        .where_json(req.r#where.as_deref())
        .build()?;

    let is_trash = req.trash.unwrap_or(false) && def.soft_delete;
    // Default sort for trash listings is a presentation concern.
    let order_by = req
        .order_by
        .clone()
        .or_else(|| is_trash.then(|| "-_deleted_at".to_string()));

    let offset = (!pagination.has_cursor()).then_some(pagination.offset);

    Ok(FindQuery::builder()
        .filters(filters)
        .order_by(order_by)
        .limit(Some(pagination.limit))
        .offset(offset)
        .select(select.map(<[String]>::to_vec))
        .after_cursor(pagination.after_cursor.clone())
        .before_cursor(pagination.before_cursor.clone())
        .search(req.search.clone())
        .build())
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Find documents in a collection with optional filters, sorting, and pagination.
    pub(in crate::api::handlers) async fn find_impl(
        &self,
        request: Request<content::FindRequest>,
    ) -> Result<Response<content::FindResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let select = if req.select.is_empty() {
            None
        } else {
            Some(req.select.clone())
        };

        let pagination = self
            .pagination_ctx
            .validate(
                req.limit,
                req.page,
                req.after_cursor.as_deref(),
                req.before_cursor.as_deref(),
            )
            .map_err(Status::invalid_argument)?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let depth = query::clamp_depth(req.depth, self.default_depth, self.max_depth);
        let cursor_enabled = self.pagination_ctx.cursor_enabled;

        let is_trash = req.trash.unwrap_or(false) && def.soft_delete;
        let find_query = build_find_query(&req, &def, &pagination, select.as_deref())?;

        let input = FindBlockingInput {
            pool: self.pool.clone(),
            runner: self.hook_runner.clone(),
            token_provider: self.token_provider.clone(),
            registry: Arc::clone(&self.registry),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            def,
            pop_cache: self.cache.clone(),
            singleflight: self.populate_singleflight.clone(),
            token,
            headers,
            find_query,
            locale_ctx,
            select,
            depth,
            cursor_enabled,
            is_trash,
            include_drafts: req.draft.unwrap_or(false),
        };

        let (proto_docs, pagination_info) = task::spawn_blocking(move || find_blocking(input))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::FindResponse {
            documents: proto_docs,
            pagination: Some(pagination_info),
        }))
    }
}
