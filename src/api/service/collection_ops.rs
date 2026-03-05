//! Collection CRUD RPC handlers: Find, FindByID, Create, Update, Delete, Count,
//! UpdateMany, DeleteMany.

use anyhow::Context as _;
use std::collections::HashMap;
use tonic::{Request, Response, Status};

use crate::api::content;
use crate::core::upload;
use crate::db::query::{AccessResult, Filter, FilterClause, FilterOp, FindQuery, LocaleContext};
use crate::db::query::filter::normalize_filter_fields;
use crate::db::{ops, query};

use super::convert::{
    document_to_proto, parse_where_json, prost_struct_to_hashmap, prost_struct_to_json_map,
};
use super::ContentService;

/// Map database/task errors to appropriate gRPC status codes.
/// Returns `Status::unavailable` for transient SQLite busy/locked/pool timeout errors
/// (enabling client retry), `Status::internal` for everything else.
fn map_db_error(e: anyhow::Error, prefix: &str) -> Status {
    let msg = e.to_string();
    let is_transient = msg.contains("database is locked")
        || msg.contains("database is busy")
        || msg.contains("SQLITE_BUSY")
        || msg.contains("SQLITE_LOCKED")
        || msg.contains("Timed out waiting")
        || msg.contains("connection pool");
    if is_transient {
        Status::unavailable(format!("{}: {} (retryable)", prefix, msg))
    } else {
        Status::internal(format!("{}: {}", prefix, msg))
    }
}

/// Untestable as unit: async methods require full ContentService with pool, registry,
/// hook_runner, and JWT secret. Covered by integration tests in tests/ directory.
#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Find documents in a collection with optional filters, sorting, and pagination.
    pub(super) async fn find_impl(
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

        // Parse `where` JSON clause
        let mut filters = if let Some(ref where_json) = req.r#where {
            parse_where_json(where_json)
                .map_err(|e| Status::invalid_argument(format!("Invalid where clause: {}", e)))?
        } else {
            Vec::new()
        };

        // Normalize dot notation: group dots → __, array/block/rel dots preserved
        normalize_filter_fields(&mut filters, &def.fields);

        // Merge access constraint filters
        if let AccessResult::Constrained(ref constraint_filters) = access_result {
            filters.extend(constraint_filters.clone());
        }

        // Draft-aware filtering: if collection has drafts and draft=false (default),
        // only return published documents
        if def.has_drafts() && !req.draft.unwrap_or(false) {
            filters.push(FilterClause::Single(Filter {
                field: "_status".to_string(),
                op: FilterOp::Equals("published".to_string()),
            }));
        }

        let select = if req.select.is_empty() {
            None
        } else {
            Some(req.select.clone())
        };

        let find_query = FindQuery {
            filters: filters.clone(),
            order_by: req.order_by,
            limit: req.limit,
            offset: req.offset,
            select: select.clone(),
        };

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
            let total = query::count(
                &conn,
                &collection,
                &def_owned,
                &filters,
                locale_ctx.as_ref(),
            )?;
            // Hydrate join table data (has-many relationships and arrays)
            let select_slice = select.as_deref();
            for doc in &mut docs {
                query::hydrate_document(&conn, &collection, &def_owned.fields, doc, select_slice, locale_ctx.as_ref())?;
            }
            // Assemble sizes for upload collections
            if let Some(ref upload_config) = def_owned.upload {
                if upload_config.enabled {
                    for doc in &mut docs {
                        upload::assemble_sizes_object(doc, upload_config);
                    }
                }
            }
            let docs = runner.apply_after_read_many(&hooks, &fields, &collection, "find", docs);
            // Populate relationships if depth > 0 (batch for efficiency)
            if depth > 0 {
                let mut docs = docs;
                let local_cache;
                let cache_ref = match &pop_cache {
                    Some(shared) => &**shared,
                    None => { local_cache = query::PopulateCache::new(); &local_cache }
                };
                query::populate_relationships_batch_cached(
                    &conn,
                    &registry,
                    &collection,
                    &def_owned,
                    &mut docs,
                    depth,
                    select_slice,
                    cache_ref,
                    locale_ctx.as_ref(),
                )?;
                return Ok((docs, total));
            }
            Ok::<_, anyhow::Error>((docs, total))
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| map_db_error(e, "Query error"))?;

        let mut proto_docs: Vec<_> = documents
            .iter()
            .map(|doc| document_to_proto(doc, &req.collection))
            .collect();

        // Strip field-level read-denied fields
        for doc in &mut proto_docs {
            self.strip_denied_read_fields(doc, &def_fields, &auth_user);
        }

        Ok(Response::new(content::FindResponse {
            documents: proto_docs,
            total,
        }))
    }

    /// Find a single document by ID with optional relationship population depth.
    pub(super) async fn find_by_id_impl(
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
        let use_draft_version = req.draft.unwrap_or(false) && def.has_drafts() && def.has_versions();

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
                &conn, &collection, &def_owned, &id,
                locale_ctx.as_ref(), access_constraints, use_draft_version,
            )?;

            // Assemble sizes for upload collections
            if let Some(ref mut d) = doc {
                if let Some(ref upload_config) = def_owned.upload {
                    if upload_config.enabled {
                        upload::assemble_sizes_object(d, upload_config);
                    }
                }
            }
            let mut doc =
                doc.map(|d| runner.apply_after_read(&hooks, &fields, &collection, "find_by_id", d));
            let select_slice = select.as_deref();
            // Populate relationships if depth > 0
            if depth > 0 {
                if let Some(ref mut d) = doc {
                    let mut visited = std::collections::HashSet::new();
                    let local_cache;
                    let cache_ref = match &pop_cache {
                        Some(shared) => &**shared,
                        None => { local_cache = query::PopulateCache::new(); &local_cache }
                    };
                    query::populate_relationships_cached(
                        &conn,
                        &registry,
                        &collection,
                        &def_owned,
                        d,
                        depth,
                        &mut visited,
                        select_slice,
                        cache_ref,
                        locale_ctx.as_ref(),
                    )?;
                }
            }
            // Apply select field stripping for find_by_id
            if let Some(ref sel) = select {
                if let Some(ref mut d) = doc {
                    query::apply_select_to_document(d, sel);
                }
            }
            Ok::<_, anyhow::Error>(doc)
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
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

    /// Create a new document, running before/after hooks within a transaction.
    pub(super) async fn create_impl(
        &self,
        request: Request<content::CreateRequest>,
    ) -> Result<Response<content::CreateResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Check create access
        let access_result =
            self.require_access(def.access.create.as_deref(), &auth_user, None, None)?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Create access denied"));
        }

        // Extract join table data (preserves structured arrays/objects)
        let join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();

        let mut data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        // Extract password for auth collections
        let password = if def.is_auth_collection() {
            data.remove("password")
        } else {
            None
        };

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let collection = req.collection.clone();
        let def_fields = def.fields.clone();
        let def_owned = def;
        let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
        let (doc, _req_context) = tokio::task::spawn_blocking(move || {
            // Strip field-level create-denied fields inside spawn_blocking
            // to avoid pool.get() on the async thread
            {
                let conn = pool.get().context("DB connection for field access")?;
                let denied = runner.check_field_write_access(
                    &def_owned.fields, user_doc.as_ref(), "create", &conn,
                );
                for name in &denied {
                    data.remove(name);
                }
            }
            crate::service::create_document(
                &pool,
                &runner,
                &collection,
                &def_owned,
                data,
                &join_data,
                password.as_deref(),
                locale_ctx.as_ref(),
                None,
                user_doc.as_ref(),
                req.draft.unwrap_or(false),
            )
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| map_db_error(e, "Create error"))?;

        if let Some(c) = &self.populate_cache { c.clear(); }

        {
            let def = self.get_collection_def(&req.collection);
            let (hooks, should_verify, live) = match &def {
                Ok(d) => (
                    d.hooks.clone(),
                    d.is_auth_collection() && d.auth.as_ref().is_some_and(|a| a.verify_email),
                    d.live.clone(),
                ),
                Err(_) => (Default::default(), false, None),
            };
            self.hook_runner.publish_event(
                &self.event_bus,
                &hooks,
                live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Create,
                req.collection.clone(),
                doc.id.clone(),
                doc.fields.clone(),
                Self::event_user_from(&auth_user),
            );

            // Auto-send verification email for auth collections with verify_email
            if should_verify {
                if let Some(user_email) = doc.fields.get("email").and_then(|v| v.as_str()) {
                    crate::service::send_verification_email(
                        self.pool.clone(),
                        self.email_config.clone(),
                        self.email_renderer.clone(),
                        self.server_config.clone(),
                        req.collection.clone(),
                        doc.id.clone(),
                        user_email.to_string(),
                    );
                }
            }
        }

        let mut proto_doc = document_to_proto(&doc, &req.collection);
        self.strip_denied_read_fields(&mut proto_doc, &def_fields, &auth_user);

        Ok(Response::new(content::CreateResponse {
            document: Some(proto_doc),
        }))
    }

    /// Update an existing document by ID, running before/after hooks within a transaction.
    pub(super) async fn update_impl(
        &self,
        request: Request<content::UpdateRequest>,
    ) -> Result<Response<content::UpdateResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Check update access
        let access_result = self.require_access(
            def.access.update.as_deref(),
            &auth_user,
            Some(&req.id),
            None,
        )?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Update access denied"));
        }

        // Extract join table data (preserves structured arrays/objects)
        let join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();

        let mut data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        let password = if def.is_auth_collection() {
            data.remove("password")
        } else {
            None
        };

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        // Handle unpublish: set status to draft, create version, return
        if req.unpublish.unwrap_or(false) && def.has_versions() {
            let pool = self.pool.clone();
            let runner = self.hook_runner.clone();
            let collection = req.collection.clone();
            let id = req.id.clone();
            let def_fields = def.fields.clone();
            let def_owned = def;
            let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
            let doc = tokio::task::spawn_blocking(move || {
                crate::service::unpublish_document(
                    &pool, &runner, &collection, &id, &def_owned, user_doc.as_ref(),
                )
            })
            .await
            .map_err(|e| Status::internal(format!("Task error: {}", e)))?
            .map_err(|e| map_db_error(e, "Unpublish error"))?;

            if let Some(c) = &self.populate_cache { c.clear(); }

            self.hook_runner.publish_event(
                &self.event_bus,
                &self.get_collection_def(&req.collection).map(|d| d.hooks.clone()).unwrap_or_default(),
                self.get_collection_def(&req.collection).ok().and_then(|d| d.live.clone()).as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Update,
                req.collection.clone(),
                req.id.clone(),
                doc.fields.clone(),
                Self::event_user_from(&auth_user),
            );

            let mut proto_doc = document_to_proto(&doc, &req.collection);
            self.strip_denied_read_fields(&mut proto_doc, &def_fields, &auth_user);

            return Ok(Response::new(content::UpdateResponse {
                document: Some(proto_doc),
            }));
        }

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let def_fields = def.fields.clone();
        let def_owned = def;
        let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
        let (doc, _req_context) = tokio::task::spawn_blocking(move || {
            // Strip field-level update-denied fields inside spawn_blocking
            // to avoid pool.get() on the async thread
            {
                let conn = pool.get().context("DB connection for field access")?;
                let denied = runner.check_field_write_access(
                    &def_owned.fields, user_doc.as_ref(), "update", &conn,
                );
                for name in &denied {
                    data.remove(name);
                }
            }
            crate::service::update_document(
                &pool,
                &runner,
                &collection,
                &id,
                &def_owned,
                data,
                &join_data,
                password.as_deref(),
                locale_ctx.as_ref(),
                None,
                user_doc.as_ref(),
                req.draft.unwrap_or(false),
            )
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| map_db_error(e, "Update error"))?;

        if let Some(c) = &self.populate_cache { c.clear(); }

        {
            let def = self.get_collection_def(&req.collection);
            let (hooks, live) = match &def {
                Ok(d) => (d.hooks.clone(), d.live.clone()),
                Err(_) => (Default::default(), None),
            };
            self.hook_runner.publish_event(
                &self.event_bus,
                &hooks,
                live.as_ref(),
                crate::core::event::EventTarget::Collection,
                crate::core::event::EventOperation::Update,
                req.collection.clone(),
                req.id.clone(),
                doc.fields.clone(),
                Self::event_user_from(&auth_user),
            );
        }

        let mut proto_doc = document_to_proto(&doc, &req.collection);
        self.strip_denied_read_fields(&mut proto_doc, &def_fields, &auth_user);

        Ok(Response::new(content::UpdateResponse {
            document: Some(proto_doc),
        }))
    }

    /// Delete a document by ID, running before/after delete hooks.
    pub(super) async fn delete_impl(
        &self,
        request: Request<content::DeleteRequest>,
    ) -> Result<Response<content::DeleteResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Check delete access
        let access_result = self.require_access(
            def.access.delete.as_deref(),
            &auth_user,
            Some(&req.id),
            None,
        )?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Delete access denied"));
        }

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let def_clone = def.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
        let config_dir = self.config_dir.clone();
        let _req_context = tokio::task::spawn_blocking(move || {
            crate::service::delete_document(
                &pool,
                &runner,
                &collection,
                &id,
                &def_clone,
                user_doc.as_ref(),
                Some(&config_dir),
            )
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| map_db_error(e, "Delete error"))?;

        if let Some(c) = &self.populate_cache { c.clear(); }

        self.hook_runner.publish_event(
            &self.event_bus,
            &def.hooks,
            def.live.as_ref(),
            crate::core::event::EventTarget::Collection,
            crate::core::event::EventOperation::Delete,
            req.collection.clone(),
            req.id.clone(),
            HashMap::new(),
            Self::event_user_from(&auth_user),
        );

        Ok(Response::new(content::DeleteResponse { success: true }))
    }

    /// Count documents matching filters (no per-document hooks).
    pub(super) async fn count_impl(
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

        let mut filters = if let Some(ref where_json) = req.r#where {
            parse_where_json(where_json)
                .map_err(|e| Status::invalid_argument(format!("Invalid where clause: {}", e)))?
        } else {
            Vec::new()
        };

        normalize_filter_fields(&mut filters, &def.fields);

        if let AccessResult::Constrained(ref constraint_filters) = access_result {
            filters.extend(constraint_filters.clone());
        }

        if def.has_drafts() && !req.draft.unwrap_or(false) {
            filters.push(FilterClause::Single(Filter {
                field: "_status".to_string(),
                op: FilterOp::Equals("published".to_string()),
            }));
        }

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let collection = req.collection.clone();
        let def_owned = def;
        let count = tokio::task::spawn_blocking(move || {
            ops::count_documents(&pool, &collection, &def_owned, &filters, locale_ctx.as_ref())
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| map_db_error(e, "Count error"))?;

        Ok(Response::new(content::CountResponse { count }))
    }

    /// Bulk update matching documents (all-or-nothing access check, no per-document hooks).
    pub(super) async fn update_many_impl(
        &self,
        request: Request<content::UpdateManyRequest>,
    ) -> Result<Response<content::UpdateManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Check read access first (to find matching docs)
        let read_access =
            self.require_access(def.access.read.as_deref(), &auth_user, None, None)?;
        if matches!(read_access, AccessResult::Denied) {
            return Err(Status::permission_denied("Read access denied"));
        }

        let mut filters = if let Some(ref where_json) = req.r#where {
            parse_where_json(where_json)
                .map_err(|e| Status::invalid_argument(format!("Invalid where clause: {}", e)))?
        } else {
            Vec::new()
        };

        normalize_filter_fields(&mut filters, &def.fields);

        if let AccessResult::Constrained(ref constraint_filters) = read_access {
            filters.extend(constraint_filters.clone());
        }

        let join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();
        let data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let hook_runner = self.hook_runner.clone();
        let collection = req.collection.clone();
        let def_owned = def.clone();
        let auth_user_clone = auth_user.clone();
        let modified = tokio::task::spawn_blocking(move || -> Result<i64, anyhow::Error> {
            let mut conn = pool.get().context("DB connection")?;
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .context("Start transaction")?;

            let find_query = FindQuery {
                filters,
                ..Default::default()
            };
            let docs = query::find(&tx, &collection, &def_owned, &find_query, locale_ctx.as_ref())?;

            // All-or-nothing update access check
            if def_owned.access.update.is_some() {
                let user_doc = auth_user_clone.as_ref().map(|au| &au.user_doc);
                for doc in &docs {
                    let result = hook_runner.check_access(
                        def_owned.access.update.as_deref(),
                        user_doc,
                        Some(&doc.id),
                        None,
                        &tx,
                    )?;
                    if matches!(result, AccessResult::Denied) {
                        anyhow::bail!("Update access denied for document {}", doc.id);
                    }
                }
            }

            let mut count = 0i64;
            for doc in &docs {
                query::update(&tx, &collection, &def_owned, &doc.id, &data, locale_ctx.as_ref())?;
                query::save_join_table_data(&tx, &collection, &def_owned.fields, &doc.id, &join_data, locale_ctx.as_ref())?;
                count += 1;
            }

            tx.commit().context("Commit transaction")?;
            Ok(count)
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| {
            if e.to_string().contains("access denied") {
                Status::permission_denied(e.to_string())
            } else {
                map_db_error(e, "UpdateMany error")
            }
        })?;

        if let Some(c) = &self.populate_cache { c.clear(); }

        Ok(Response::new(content::UpdateManyResponse { modified }))
    }

    /// Bulk delete matching documents (all-or-nothing access check, no per-document hooks).
    pub(super) async fn delete_many_impl(
        &self,
        request: Request<content::DeleteManyRequest>,
    ) -> Result<Response<content::DeleteManyResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        // Check read access first (to find matching docs)
        let read_access =
            self.require_access(def.access.read.as_deref(), &auth_user, None, None)?;
        if matches!(read_access, AccessResult::Denied) {
            return Err(Status::permission_denied("Read access denied"));
        }

        let mut filters = if let Some(ref where_json) = req.r#where {
            parse_where_json(where_json)
                .map_err(|e| Status::invalid_argument(format!("Invalid where clause: {}", e)))?
        } else {
            Vec::new()
        };

        normalize_filter_fields(&mut filters, &def.fields);

        if let AccessResult::Constrained(ref constraint_filters) = read_access {
            filters.extend(constraint_filters.clone());
        }

        let pool = self.pool.clone();
        let hook_runner = self.hook_runner.clone();
        let collection = req.collection.clone();
        let def_owned = def.clone();
        let auth_user_clone = auth_user.clone();
        let deleted = tokio::task::spawn_blocking(move || -> Result<i64, anyhow::Error> {
            let mut conn = pool.get().context("DB connection")?;
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .context("Start transaction")?;

            let find_query = FindQuery {
                filters,
                ..Default::default()
            };
            let docs = query::find(&tx, &collection, &def_owned, &find_query, None)?;

            // All-or-nothing delete access check
            if def_owned.access.delete.is_some() {
                let user_doc = auth_user_clone.as_ref().map(|au| &au.user_doc);
                for doc in &docs {
                    let result = hook_runner.check_access(
                        def_owned.access.delete.as_deref(),
                        user_doc,
                        Some(&doc.id),
                        None,
                        &tx,
                    )?;
                    if matches!(result, AccessResult::Denied) {
                        anyhow::bail!("Delete access denied for document {}", doc.id);
                    }
                }
            }

            let mut count = 0i64;
            for doc in &docs {
                query::delete(&tx, &collection, &doc.id)?;
                count += 1;
            }

            tx.commit().context("Commit transaction")?;
            Ok(count)
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| {
            if e.to_string().contains("access denied") {
                Status::permission_denied(e.to_string())
            } else {
                map_db_error(e, "DeleteMany error")
            }
        })?;

        if let Some(c) = &self.populate_cache { c.clear(); }

        Ok(Response::new(content::DeleteManyResponse { deleted }))
    }
}
