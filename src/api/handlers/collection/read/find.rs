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
fn build_find_query(
    req: &content::FindRequest,
    def: &crate::core::CollectionDefinition,
    pagination: &query::FindPagination,
    select: Option<&[String]>,
) -> Result<FindQuery, Status> {
    let filters = FilterBuilder::new(&def.fields, &AccessResult::Allowed)
        .where_json(req.r#where.as_deref())
        .draft_filter(def.has_drafts(), !req.draft.unwrap_or(false))
        .build()?;

    let mut fq = FindQuery::builder()
        .filters(filters)
        .limit(pagination.limit);

    if let Some(ref ob) = req.order_by {
        fq = fq.order_by(ob.clone());
    }

    if !pagination.has_cursor() {
        fq = fq.offset(pagination.offset);
    }

    if let Some(s) = select {
        fq = fq.select(s.to_vec());
    }

    if let Some(ref c) = pagination.after_cursor {
        fq = fq.after_cursor(c.clone());
    }

    if let Some(ref c) = pagination.before_cursor {
        fq = fq.before_cursor(c.clone());
    }

    if let Some(ref s) = req.search {
        fq = fq.search(s.clone());
    }

    Ok(fq.build())
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
        let def_owned = def;

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
