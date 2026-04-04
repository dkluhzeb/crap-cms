//! Find handler — query documents with filters, sorting, and pagination.

use std::collections::HashMap;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        service::{
            ContentService,
            collection::{filter_builder::FilterBuilder, helpers::map_db_error},
            convert::document_to_proto,
        },
    },
    db::{AccessResult, FindQuery, LocaleContext, query},
};

use crate::api::service::collection::helpers::strip_read_denied_proto_fields;

use super::helpers::{PostProcessCtx, pagination_result_to_proto, post_process_docs};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Find documents in a collection with optional filters, sorting, and pagination.
    pub(in crate::api::service) async fn find_impl(
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
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);
        let depth = req.depth.unwrap_or(0).max(0).min(self.max_depth);
        let cursor_enabled = self.pagination_ctx.cursor_enabled;
        let has_timestamps = def.timestamps;

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let hooks = def.hooks.clone();
        let def_fields = def.fields.clone();
        let fields = def_fields.clone();
        let collection = req.collection.clone();
        let pop_cache = self.cache.clone();
        let req_where = req.r#where.clone();
        let has_drafts = def.has_drafts();
        let draft = req.draft;
        let order_by = req.order_by.clone();
        let search = req.search.clone();
        let def_owned = def;

        let (proto_docs, pagination_info) = task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            let access_result = ContentService::check_access_blocking(
                def_owned.access.read.as_deref(),
                &auth_user,
                None,
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Read access denied"));
            }

            let filters = FilterBuilder::new(&def_owned.fields, &access_result)
                .where_json(req_where.as_deref())
                .draft_filter(has_drafts, !draft.unwrap_or(false))
                .build()?;

            let mut find_query = FindQuery::new();
            find_query.filters = filters.clone();
            find_query.order_by = order_by.clone();
            find_query.limit = Some(pagination.limit);
            find_query.offset = if pagination.has_cursor() {
                None
            } else {
                Some(pagination.offset)
            };
            find_query.select = select.clone();
            find_query.after_cursor = pagination.after_cursor.clone();
            find_query.before_cursor = pagination.before_cursor.clone();
            find_query.search = search;

            query::validate_query_fields(&def_owned, &find_query, locale_ctx.as_ref())
                .map_err(|e| Status::invalid_argument(e.to_string()))?;

            runner
                .fire_before_read(&hooks, &collection, "find", HashMap::new())
                .map_err(|e| map_db_error(e, "Query error", &db_kind))?;

            let mut docs = query::find(
                &conn,
                &collection,
                &def_owned,
                &find_query,
                locale_ctx.as_ref(),
            )
            .map_err(|e| map_db_error(e, "Query error", &db_kind))?;

            let total = query::count_with_search(
                &conn,
                &collection,
                &def_owned,
                &filters,
                locale_ctx.as_ref(),
                find_query.search.as_deref(),
                find_query.include_deleted,
            )
            .map_err(|e| map_db_error(e, "Count error", &db_kind))?;

            let select_slice = select.as_deref();
            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);

            let pp_ctx = PostProcessCtx {
                conn: &conn,
                collection: &collection,
                def: &def_owned,
                select: select_slice,
                locale_ctx: locale_ctx.as_ref(),
                runner: &runner,
                hooks: &hooks,
                fields: &fields,
                user_doc,
                db_kind: &db_kind,
            };

            post_process_docs(&mut docs, &pp_ctx)?;

            if depth > 0 {
                let cache_ref = &*pop_cache;
                let pop_ctx =
                    query::PopulateContext::new(&conn, &registry, &collection, &def_owned);
                let mut pop_opts = query::PopulateOpts::new(depth);

                if let Some(s) = select_slice {
                    pop_opts = pop_opts.select(s);
                }

                if let Some(ref lc) = locale_ctx {
                    pop_opts = pop_opts.locale_ctx(lc);
                }

                query::populate_relationships_batch_cached(
                    &pop_ctx, &mut docs, &pop_opts, cache_ref,
                )
                .map_err(|e| map_db_error(e, "Query error", &db_kind))?;
            }

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
        .map_err(|e| {
            error!("Task error: {}", e);
            Status::internal("Internal error")
        })??;

        Ok(Response::new(content::FindResponse {
            documents: proto_docs,
            pagination: Some(pagination_info),
        }))
    }
}
