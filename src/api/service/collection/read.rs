//! Read-oriented collection RPC handlers: Find, FindByID, Count.

use anyhow::Context as _;
use std::collections::HashMap;
use tonic::{Request, Response, Status};

use crate::api::content;
use crate::api::service::ContentService;
use crate::api::service::convert::document_to_proto;
use crate::core::upload;
use crate::db::query::{AccessResult, FindQuery, LocaleContext};
use crate::db::{ops, query};

use super::filter_builder::FilterBuilder;
use super::helpers::map_db_error;
use super::pagination_builder::PaginationBuilder;

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
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Check read access
        let access_result =
            self.require_access(def.access.read.as_deref(), &auth_user, None, None)?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Read access denied"));
        }

        let filters = FilterBuilder::new(&def.fields, &access_result)
            .where_json(req.r#where.as_deref())
            .draft_filter(def.has_drafts(), !req.draft.unwrap_or(false))
            .build()?;

        let select = if req.select.is_empty() {
            None
        } else {
            Some(req.select.clone())
        };

        // Clamp limit to configured bounds
        let clamped_limit =
            query::apply_pagination_limits(req.limit, self.default_limit, self.max_limit);

        // Convert page → internal offset (default page=1)
        let page = req.page.unwrap_or(1).max(1);
        let internal_offset = (page - 1) * clamped_limit;

        // Cursor decoding and validation
        let (after_cursor, before_cursor) = if self.cursor_enabled {
            let ac = if let Some(ref s) = req.after_cursor {
                if req.page.is_some() {
                    return Err(Status::invalid_argument(
                        "Cannot use both after_cursor and page — they are mutually exclusive",
                    ));
                }
                Some(
                    query::cursor::CursorData::decode(s)
                        .map_err(|e| Status::invalid_argument(format!("Invalid cursor: {}", e)))?,
                )
            } else {
                None
            };
            let bc = if let Some(ref s) = req.before_cursor {
                if req.page.is_some() {
                    return Err(Status::invalid_argument(
                        "Cannot use both before_cursor and page — they are mutually exclusive",
                    ));
                }
                if ac.is_some() {
                    return Err(Status::invalid_argument(
                        "Cannot use both after_cursor and before_cursor — they are mutually exclusive",
                    ));
                }
                Some(
                    query::cursor::CursorData::decode(s)
                        .map_err(|e| Status::invalid_argument(format!("Invalid cursor: {}", e)))?,
                )
            } else {
                None
            };
            (ac, bc)
        } else {
            (None, None)
        };

        let has_cursor = after_cursor.is_some() || before_cursor.is_some();

        let mut find_query = FindQuery::new();
        find_query.filters = filters.clone();
        find_query.order_by = req.order_by.clone();
        find_query.limit = Some(clamped_limit);
        find_query.offset = if has_cursor {
            None
        } else {
            Some(internal_offset)
        };
        find_query.select = select.clone();
        find_query.after_cursor = after_cursor.clone();
        find_query.before_cursor = before_cursor.clone();
        find_query.search = req.search.clone();

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        // Validate filter/order_by fields early for a clear INVALID_ARGUMENT status
        query::validate_query_fields(&def, &find_query, locale_ctx.as_ref())
            .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let depth = req.depth.unwrap_or(0).max(0).min(self.max_depth);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let hooks = def.hooks.clone();
        let def_fields = def.fields.clone();
        let fields = def_fields.clone();
        let has_timestamps = def.timestamps;
        let collection = req.collection.clone();
        let registry = self.registry.clone();
        let pop_cache = self.populate_cache.clone();
        let def_owned = def;
        let (documents, total) = tokio::task::spawn_blocking(move || {
            runner.fire_before_read(&hooks, &collection, "find", HashMap::new())?;
            // Single connection for find + count + hydration + population
            let conn = pool.get().context("DB connection")?;
            let mut docs = query::find(
                &conn,
                &collection,
                &def_owned,
                &find_query,
                locale_ctx.as_ref(),
            )?;
            let total = query::count_with_search(
                &conn,
                &collection,
                &def_owned,
                &filters,
                locale_ctx.as_ref(),
                find_query.search.as_deref(),
            )?;
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
                )?;
            }
            // Assemble sizes for upload collections
            if let Some(ref upload_config) = def_owned.upload
                && upload_config.enabled
            {
                for doc in &mut docs {
                    upload::assemble_sizes_object(doc, upload_config);
                }
            }
            let ar_ctx = crate::hooks::lifecycle::AfterReadCtx {
                hooks: &hooks,
                fields: &fields,
                collection: &collection,
                operation: "find",
                user: None,
                ui_locale: None,
            };
            let docs = runner.apply_after_read_many(&ar_ctx, docs);
            // Populate relationships if depth > 0 (batch for efficiency)
            if depth > 0 {
                let mut docs = docs;
                let local_cache;
                let cache_ref = match &pop_cache {
                    Some(shared) => &**shared,
                    None => {
                        local_cache = query::PopulateCache::new();
                        &local_cache
                    }
                };
                let pop_ctx = query::PopulateContext {
                    conn: &conn,
                    registry: &registry,
                    collection_slug: &collection,
                    def: &def_owned,
                };
                let pop_opts = query::PopulateOpts {
                    depth,
                    select: select_slice,
                    locale_ctx: locale_ctx.as_ref(),
                };
                query::populate_relationships_batch_cached(
                    &pop_ctx, &mut docs, &pop_opts, cache_ref,
                )?;
                return Ok((docs, total));
            }
            Ok::<_, anyhow::Error>((docs, total))
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })?
        .map_err(|e| map_db_error(e, "Query error"))?;

        let mut proto_docs: Vec<_> = documents
            .iter()
            .map(|doc| document_to_proto(doc, &req.collection))
            .collect();

        // Strip field-level read-denied fields
        for doc in &mut proto_docs {
            self.strip_denied_read_fields(doc, &def_fields, &auth_user);
        }

        // Build PaginationInfo
        let pagination = if self.cursor_enabled {
            PaginationBuilder::new(&documents, total, clamped_limit)
                .cursor_mode(req.order_by.as_deref(), has_timestamps)
                .cursor_state(before_cursor.is_some(), has_cursor)
                .build()
        } else {
            PaginationBuilder::new(&documents, total, clamped_limit)
                .page(page, internal_offset)
                .build()
        };

        Ok(Response::new(content::FindResponse {
            documents: proto_docs,
            pagination: Some(pagination),
        }))
    }

    /// Find a single document by ID with optional relationship population depth.
    pub(in crate::api::service) async fn find_by_id_impl(
        &self,
        request: Request<content::FindByIdRequest>,
    ) -> Result<Response<content::FindByIdResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Check read access
        let access_result =
            self.require_access(def.access.read.as_deref(), &auth_user, Some(&req.id), None)?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Read access denied"));
        }

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
        let hooks = def.hooks.clone();
        let fields = def.fields.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let access_constraints = if let AccessResult::Constrained(ref filters) = access_result {
            Some(filters.clone())
        } else {
            None
        };
        let def_fields = def.fields.clone();
        let registry = self.registry.clone();
        let pop_cache = self.populate_cache.clone();
        let def_owned = def;
        let doc = tokio::task::spawn_blocking(move || {
            runner.fire_before_read(&hooks, &collection, "find_by_id", HashMap::new())?;

            let conn = pool.get().context("DB connection")?;
            let mut doc = ops::find_by_id_full(
                &conn,
                &collection,
                &def_owned,
                &id,
                locale_ctx.as_ref(),
                access_constraints,
                use_draft_version,
            )?;

            // Assemble sizes for upload collections
            if let Some(ref mut d) = doc
                && let Some(ref upload_config) = def_owned.upload
                && upload_config.enabled
            {
                upload::assemble_sizes_object(d, upload_config);
            }
            let ar_ctx = crate::hooks::lifecycle::AfterReadCtx {
                hooks: &hooks,
                fields: &fields,
                collection: &collection,
                operation: "find_by_id",
                user: None,
                ui_locale: None,
            };
            let mut doc = doc.map(|d| runner.apply_after_read(&ar_ctx, d));
            let select_slice = select.as_deref();
            // Populate relationships if depth > 0
            if depth > 0
                && let Some(ref mut d) = doc
            {
                let mut visited = std::collections::HashSet::new();
                let local_cache;
                let cache_ref = match &pop_cache {
                    Some(shared) => &**shared,
                    None => {
                        local_cache = query::PopulateCache::new();
                        &local_cache
                    }
                };
                let pop_ctx = query::PopulateContext {
                    conn: &conn,
                    registry: &registry,
                    collection_slug: &collection,
                    def: &def_owned,
                };
                let pop_opts = query::PopulateOpts {
                    depth,
                    select: select_slice,
                    locale_ctx: locale_ctx.as_ref(),
                };
                query::populate_relationships_cached(
                    &pop_ctx,
                    d,
                    &mut visited,
                    &pop_opts,
                    cache_ref,
                )?;
            }
            // Apply select field stripping for find_by_id
            if let Some(ref sel) = select
                && let Some(ref mut d) = doc
            {
                query::apply_select_to_document(d, sel);
            }
            Ok::<_, anyhow::Error>(doc)
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })?
        .map_err(|e| map_db_error(e, "Query error"))?;

        let mut proto_doc = doc.map(|d| document_to_proto(&d, &req.collection));

        // Strip field-level read-denied fields
        if let Some(ref mut d) = proto_doc {
            self.strip_denied_read_fields(d, &def_fields, &auth_user);
        }

        Ok(Response::new(content::FindByIdResponse {
            document: proto_doc,
        }))
    }

    /// Count documents matching filters (no per-document hooks).
    pub(in crate::api::service) async fn count_impl(
        &self,
        request: Request<content::CountRequest>,
    ) -> Result<Response<content::CountResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Check read access
        let access_result =
            self.require_access(def.access.read.as_deref(), &auth_user, None, None)?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Read access denied"));
        }

        let filters = FilterBuilder::new(&def.fields, &access_result)
            .where_json(req.r#where.as_deref())
            .draft_filter(def.has_drafts(), !req.draft.unwrap_or(false))
            .build()?;

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let collection = req.collection.clone();
        let def_owned = def;
        let search = req.search.clone();
        let count = tokio::task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;
            query::count_with_search(
                &conn,
                &collection,
                &def_owned,
                &filters,
                locale_ctx.as_ref(),
                search.as_deref(),
            )
        })
        .await
        .map_err(|e| {
            tracing::error!("Task error: {}", e);
            Status::internal("Internal error")
        })?
        .map_err(|e| map_db_error(e, "Count error"))?;

        Ok(Response::new(content::CountResponse { count }))
    }
}
