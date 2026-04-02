//! Read-oriented collection RPC handlers: Find, FindByID, Count.

use std::collections::HashMap;
use tonic::{Request, Response, Status};

use crate::{
    api::{
        content,
        service::{ContentService, convert::document_to_proto},
    },
    core::upload,
    db::{
        AccessResult, FindQuery, LocaleContext, ops,
        query::{self},
    },
    hooks::lifecycle::AfterReadCtx,
};

use super::{
    filter_builder::FilterBuilder,
    helpers::{map_db_error, strip_denied_proto_fields},
};

/// Untestable as unit: async methods require full ContentService with pool, registry,
/// hook_runner, and JWT secret. Covered by integration tests in tests/ directory.
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
        let (proto_docs, pagination_info) =
            tokio::task::spawn_blocking(move || -> Result<_, Status> {
                let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

                // Auth + access (all on blocking thread)
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

                // Hydrate join table data (has-many relationships and arrays)
                let select_slice = select.as_deref();
                for doc in &mut docs {
                    query::hydrate_document(
                        &conn,
                        &collection,
                        &def_owned.fields,
                        doc,
                        select_slice,
                        locale_ctx.as_ref(),
                    )
                    .map_err(|e| map_db_error(e, "Query error", &db_kind))?;
                }

                // Assemble sizes for upload collections
                if let Some(ref upload_config) = def_owned.upload
                    && upload_config.enabled
                {
                    for doc in &mut docs {
                        upload::assemble_sizes_object(doc, upload_config);
                    }
                }

                let ar_ctx = AfterReadCtx {
                    hooks: &hooks,
                    fields: &fields,
                    collection: &collection,
                    operation: "find",
                    user: auth_user.as_ref().map(|au| &au.user_doc),
                    ui_locale: None,
                };
                let mut docs = runner.apply_after_read_many(&ar_ctx, docs);

                // Populate relationships if depth > 0 (batch for efficiency)
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

                // Proto conversion
                let mut proto_docs: Vec<_> = docs
                    .iter()
                    .map(|doc| document_to_proto(doc, &collection))
                    .collect();

                // Strip field-level read-denied fields (using existing conn)
                let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
                let tx = conn.transaction().map_err(|e| {
                    tracing::error!("Field access check tx error: {}", e);
                    Status::internal("Internal error")
                })?;
                let denied = runner.check_field_read_access(&def_fields, user_doc, &tx);
                if let Err(e) = tx.commit() {
                    tracing::warn!("tx commit failed: {e}");
                }
                for doc in &mut proto_docs {
                    strip_denied_proto_fields(doc, &denied);
                }

                // Build pagination
                let pr = if cursor_enabled {
                    // When docs < limit, we know there are no more pages in this direction.
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
                let pagination_info = pagination_result_to_proto(&pr);

                Ok((proto_docs, pagination_info))
            })
            .await
            .map_err(|e| {
                tracing::error!("Task error: {}", e);
                Status::internal("Internal error")
            })??;

        Ok(Response::new(content::FindResponse {
            documents: proto_docs,
            pagination: Some(pagination_info),
        }))
    }

    /// Find a single document by ID with optional relationship population depth.
    pub(in crate::api::service) async fn find_by_id_impl(
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
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        // Draft-aware find_by_id: if draft=true and collection has drafts,
        // load the latest version snapshot instead of the main table document
        let use_draft_version =
            req.draft.unwrap_or(false) && def.has_drafts() && def.has_versions();

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let hooks = def.hooks.clone();
        let fields = def.fields.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let def_fields = def.fields.clone();
        let pop_cache = self.cache.clone();
        let def_owned = def;
        let result = tokio::task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            // Auth + access (all on blocking thread)
            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;
            let access_result = ContentService::check_access_blocking(
                def_owned.access.read.as_deref(),
                &auth_user,
                Some(&id),
                None,
                &runner,
                &mut conn,
            )?;

            if matches!(access_result, AccessResult::Denied) {
                return Err(Status::permission_denied("Read access denied"));
            }

            let access_constraints = if let AccessResult::Constrained(ref filters) = access_result {
                Some(filters.clone())
            } else {
                None
            };

            runner
                .fire_before_read(&hooks, &collection, "find_by_id", HashMap::new())
                .map_err(|e| map_db_error(e, "Query error", &db_kind))?;

            let mut doc = ops::find_by_id_full(
                &conn,
                &collection,
                &def_owned,
                &id,
                locale_ctx.as_ref(),
                access_constraints,
                use_draft_version,
            )
            .map_err(|e| map_db_error(e, "Query error", &db_kind))?;

            // Assemble sizes for upload collections
            if let Some(ref mut d) = doc
                && let Some(ref upload_config) = def_owned.upload
                && upload_config.enabled
            {
                upload::assemble_sizes_object(d, upload_config);
            }

            let ar_ctx = AfterReadCtx {
                hooks: &hooks,
                fields: &fields,
                collection: &collection,
                operation: "find_by_id",
                user: auth_user.as_ref().map(|au| &au.user_doc),
                ui_locale: None,
            };
            let mut doc = doc.map(|d| runner.apply_after_read(&ar_ctx, d));
            let select_slice = select.as_deref();

            // Populate relationships if depth > 0
            if depth > 0
                && let Some(ref mut d) = doc
            {
                let mut visited = std::collections::HashSet::new();
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
                query::populate_relationships_cached(
                    &pop_ctx,
                    d,
                    &mut visited,
                    &pop_opts,
                    cache_ref,
                )
                .map_err(|e| map_db_error(e, "Query error", &db_kind))?;
            }

            // Apply select field stripping for find_by_id
            if let Some(ref sel) = select
                && let Some(ref mut d) = doc
            {
                query::apply_select_to_document(d, sel);
            }

            match doc {
                Some(d) => {
                    let mut proto_doc = document_to_proto(&d, &collection);

                    // Strip field-level read-denied fields (using existing conn)
                    let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
                    let tx = conn.transaction().map_err(|e| {
                        tracing::error!("Field access check tx error: {}", e);
                        Status::internal("Internal error")
                    })?;
                    let denied = runner.check_field_read_access(&def_fields, user_doc, &tx);
                    if let Err(e) = tx.commit() {
                        tracing::warn!("tx commit failed: {e}");
                    }
                    strip_denied_proto_fields(&mut proto_doc, &denied);

                    Ok(Some(proto_doc))
                }
                None => Err(Status::not_found(format!(
                    "Document '{}' not found in '{}'",
                    id, collection
                ))),
            }
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })??;

        Ok(Response::new(content::FindByIdResponse {
            document: result,
        }))
    }

    /// Count documents matching filters (no per-document hooks).
    pub(in crate::api::service) async fn count_impl(
        &self,
        request: Request<content::CountRequest>,
    ) -> Result<Response<content::CountResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let req_where = req.r#where.clone();
        let has_drafts = def.has_drafts();
        let draft = req.draft;
        let search = req.search.clone();
        let def_owned = def;
        let count = tokio::task::spawn_blocking(move || -> Result<_, Status> {
            let mut conn = pool.get().map_err(|e| map_db_error(e, "Pool", &db_kind))?;

            // Auth + access (all on blocking thread)
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

            query::count_with_search(
                &conn,
                &collection,
                &def_owned,
                &filters,
                locale_ctx.as_ref(),
                search.as_deref(),
                false,
            )
            .map_err(|e| map_db_error(e, "Count error", &db_kind))
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })??;

        Ok(Response::new(content::CountResponse { count }))
    }
}

/// Convert a [`query::PaginationResult`] to a gRPC `PaginationInfo` message.
fn pagination_result_to_proto(pr: &query::PaginationResult) -> content::PaginationInfo {
    content::PaginationInfo {
        total_docs: pr.total_docs,
        limit: pr.limit,
        total_pages: pr.total_pages,
        page: pr.page,
        page_start: pr.page_start,
        has_prev_page: pr.has_prev_page,
        has_next_page: pr.has_next_page,
        prev_page: pr.prev_page,
        next_page: pr.next_page,
        start_cursor: pr.start_cursor.clone(),
        end_cursor: pr.end_cursor.clone(),
    }
}
