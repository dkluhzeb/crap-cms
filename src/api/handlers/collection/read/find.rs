//! Find handler — query documents with filters, sorting, and pagination.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{
            ContentService,
            collection::{filter_builder::FilterBuilder, helpers::strip_read_denied_proto_fields},
            convert::document_to_proto,
        },
    },
    db::{AccessResult, FindQuery, LocaleContext, query},
    service::{ReadOptions, RunnerReadHooks, ServiceError, find_documents},
};

use super::helpers::pagination_result_to_proto;

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
        let has_timestamps = def.timestamps;

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let def_fields = def.fields.clone();
        let collection = req.collection.clone();
        let pop_cache = self.cache.clone();
        let req_where = req.r#where.clone();
        let has_drafts = def.has_drafts();
        let draft = req.draft;
        let order_by = req.order_by.clone();
        let search = req.search.clone();
        let def_owned = def;

        let (proto_docs, pagination_info) = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            // Access check is handled by service::find_documents — pass Allowed to FilterBuilder
            let filters = FilterBuilder::new(&def_owned.fields, &AccessResult::Allowed)
                .where_json(req_where.as_deref())
                .draft_filter(has_drafts, !draft.unwrap_or(false))
                .build()?;

            let mut fq_builder = FindQuery::builder()
                .filters(filters.clone())
                .limit(pagination.limit);

            if let Some(ref ob) = order_by {
                fq_builder = fq_builder.order_by(ob.clone());
            }

            if !pagination.has_cursor() {
                fq_builder = fq_builder.offset(pagination.offset);
            }

            if let Some(ref s) = select {
                fq_builder = fq_builder.select(s.clone());
            }

            if let Some(ref c) = pagination.after_cursor {
                fq_builder = fq_builder.after_cursor(c.clone());
            }

            if let Some(ref c) = pagination.before_cursor {
                fq_builder = fq_builder.before_cursor(c.clone());
            }

            if let Some(s) = search {
                fq_builder = fq_builder.search(s);
            }

            let find_query = fq_builder.build();

            query::validate_query_fields(&def_owned, &find_query, locale_ctx.as_ref())
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

            let select_slice = select.as_deref();
            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

            let read_hooks = RunnerReadHooks::new(&runner, &conn);
            let read_opts = ReadOptions::builder()
                .depth(depth)
                .select(select_slice)
                .locale_ctx(locale_ctx.as_ref())
                .registry(Some(&registry))
                .user(user_doc)
                .cache(Some(&*pop_cache))
                .build();

            let result = find_documents(
                &conn,
                &read_hooks,
                &collection,
                &def_owned,
                &find_query,
                &read_opts,
            )
            .map_err(Status::from)?;

            let docs = result.docs;
            let total = result.total;

            let mut proto_docs: Vec<_> = docs
                .iter()
                .map(|doc| document_to_proto(doc, &collection))
                .collect();

            strip_read_denied_proto_fields(
                &mut proto_docs,
                &mut conn,
                &runner,
                &def_fields,
                user_doc,
            );

            let pr = if cursor_enabled {
                let cursor_has_more =
                    if pagination.has_cursor() && (docs.len() as i64) < pagination.limit {
                        Some(false)
                    } else {
                        None
                    };

                query::PaginationResult::builder(&docs, total, pagination.limit).cursor(
                    order_by.as_deref(),
                    has_timestamps,
                    pagination.before_cursor.is_some(),
                    pagination.has_cursor(),
                    cursor_has_more,
                )
            } else {
                query::PaginationResult::builder(&docs, total, pagination.limit)
                    .page(pagination.page, pagination.offset)
            };

            Ok((proto_docs, pagination_result_to_proto(&pr)))
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
