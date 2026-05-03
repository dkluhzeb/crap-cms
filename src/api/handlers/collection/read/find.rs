//! Find handler — query documents with filters, sorting, and pagination.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{
            ContentService, collection::filter_builder::FilterBuilder, convert::document_to_proto,
        },
    },
    db::{AccessResult, FindQuery, LocaleContext, query},
    service::{FindDocumentsInput, RunnerReadHooks, ServiceContext, ServiceError, find_documents},
};

use crate::api::handlers::convert::pagination_result_to_proto;

/// Build a FindQuery from the gRPC request parameters.
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
        let depth = req.depth.unwrap_or(0).max(0).min(self.max_depth);
        let cursor_enabled = self.pagination_ctx.cursor_enabled;

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let pop_cache = self.cache.clone();
        let singleflight = self.populate_singleflight.clone();
        let def_owned = def;
        let is_trash = req.trash.unwrap_or(false) && def_owned.soft_delete;
        let include_drafts = req.draft.unwrap_or(false);

        let find_query = build_find_query(&req, &def_owned, &pagination, select.as_deref())?;

        let (proto_docs, pagination_info) = task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            query::validate_query_fields(&def_owned, &find_query, locale_ctx.as_ref())
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

            let read_hooks = RunnerReadHooks::new(&runner, &conn);
            let ctx = ServiceContext::collection(&collection, &def_owned)
                .pool(&pool)
                .conn(&conn)
                .read_hooks(&read_hooks)
                .user(user_doc)
                .build();

            let input = FindDocumentsInput::builder(&find_query)
                .depth(depth)
                .select(select.as_deref())
                .locale_ctx(locale_ctx.as_ref())
                .registry(Some(&registry))
                .cache(Some(&*pop_cache))
                .cursor_enabled(cursor_enabled)
                .trash(is_trash)
                .include_drafts(include_drafts)
                .singleflight(Some(singleflight))
                .build();

            let result = find_documents(&ctx, &input).map_err(Status::from)?;

            let proto_docs: Vec<_> = result
                .docs
                .iter()
                .map(|doc| document_to_proto(doc, &collection))
                .collect();

            Ok((proto_docs, pagination_result_to_proto(&result.pagination)))
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::FindResponse {
            documents: proto_docs,
            pagination: Some(pagination_info),
        }))
    }
}
