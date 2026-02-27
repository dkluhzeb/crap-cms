//! Tonic gRPC service implementing all ContentAPI RPCs.

mod auth;
mod convert;

use anyhow::Context as _;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::pin::Pin;
use tokio_stream::{wrappers::BroadcastStream, Stream, StreamExt};
use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status};

use crate::api::content;
use crate::api::content::content_api_server::ContentApi;
use crate::config::{EmailConfig, LocaleConfig, ServerConfig};
use crate::core::auth::AuthUser;
use crate::core::email::EmailRenderer;
use crate::core::event::EventBus;
use crate::core::rate_limit::LoginRateLimiter;
use crate::core::upload;
use crate::core::SharedRegistry;
use crate::db::query::{AccessResult, Filter, FilterClause, FilterOp, FindQuery, LocaleContext};
use crate::db::query::filter::normalize_filter_fields;
use crate::db::DbPool;
use crate::db::{ops, query};
use crate::core::event::EventUser;
use crate::hooks::lifecycle::HookRunner;

use convert::{
    document_to_proto, field_def_to_proto, json_to_prost_value, parse_where_json,
    prost_struct_to_hashmap, prost_struct_to_json_map,
};

/// Implements the gRPC ContentAPI service (Find, Create, Update, Delete, Login, etc.).
pub struct ContentService {
    pool: DbPool,
    registry: SharedRegistry,
    hook_runner: HookRunner,
    jwt_secret: String,
    default_depth: i32,
    max_depth: i32,
    email_config: EmailConfig,
    email_renderer: std::sync::Arc<EmailRenderer>,
    server_config: ServerConfig,
    event_bus: Option<EventBus>,
    locale_config: LocaleConfig,
    config_dir: std::path::PathBuf,
    login_limiter: std::sync::Arc<LoginRateLimiter>,
}

/// Untestable as unit: helper methods require full pool + registry + hook_runner.
/// Covered by integration tests in tests/ directory.
#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Create a new gRPC content service with all dependencies.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: DbPool,
        registry: SharedRegistry,
        hook_runner: HookRunner,
        jwt_secret: String,
        depth_config: &crate::config::DepthConfig,
        email_config: EmailConfig,
        email_renderer: std::sync::Arc<EmailRenderer>,
        server_config: ServerConfig,
        event_bus: Option<EventBus>,
        locale_config: LocaleConfig,
        config_dir: std::path::PathBuf,
        login_limiter: std::sync::Arc<LoginRateLimiter>,
    ) -> Self {
        Self {
            pool,
            registry,
            hook_runner,
            jwt_secret,
            default_depth: depth_config.default_depth,
            max_depth: depth_config.max_depth,
            email_config,
            email_renderer,
            server_config,
            event_bus,
            locale_config,
            config_dir,
            login_limiter,
        }
    }

    #[allow(clippy::result_large_err)]
    fn get_collection_def(&self, slug: &str) -> Result<crate::core::CollectionDefinition, Status> {
        let reg = self
            .registry
            .read()
            .map_err(|e| Status::internal(format!("Registry lock poisoned: {}", e)))?;
        reg.get_collection(slug)
            .cloned()
            .ok_or_else(|| Status::not_found(format!("Collection '{}' not found", slug)))
    }

    #[allow(clippy::result_large_err)]
    fn get_global_def(
        &self,
        slug: &str,
    ) -> Result<crate::core::collection::GlobalDefinition, Status> {
        let reg = self
            .registry
            .read()
            .map_err(|e| Status::internal(format!("Registry lock poisoned: {}", e)))?;
        reg.get_global(slug)
            .cloned()
            .ok_or_else(|| Status::not_found(format!("Global '{}' not found", slug)))
    }

    /// Extract auth user from gRPC metadata (Bearer token in `authorization` header).
    fn extract_auth_user(&self, metadata: &MetadataMap) -> Option<AuthUser> {
        let token = metadata
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))?;
        let claims = crate::core::auth::validate_token(token, &self.jwt_secret).ok()?;
        let def = {
            let reg = self.registry.read().ok()?;
            reg.get_collection(&claims.collection)?.clone()
        };
        let conn = self.pool.get().ok()?;
        let doc = query::find_by_id(&conn, &claims.collection, &def, &claims.sub, None).ok()??;
        Some(AuthUser {
            claims,
            user_doc: doc,
        })
    }

    /// Check collection-level access, returning the AccessResult or a Status error.
    #[allow(clippy::result_large_err)]
    fn require_access(
        &self,
        access_ref: Option<&str>,
        auth_user: &Option<AuthUser>,
        id: Option<&str>,
        data: Option<&HashMap<String, serde_json::Value>>,
    ) -> Result<AccessResult, Status> {
        let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
        let conn = self
            .pool
            .get()
            .map_err(|_| Status::internal("Database connection error"))?;
        self.hook_runner
            .check_access(access_ref, user_doc, id, data, &conn)
            .map_err(|e| Status::internal(format!("Access check error: {}", e)))
    }

    /// Extract an EventUser from the gRPC AuthUser (for SSE event attribution).
    fn event_user_from(auth_user: &Option<AuthUser>) -> Option<EventUser> {
        auth_user.as_ref().map(|au| EventUser {
            id: au.claims.sub.clone(),
            email: au.claims.email.clone(),
        })
    }

    /// Strip field-level read-denied fields from a proto document.
    fn strip_denied_read_fields(
        &self,
        doc: &mut content::Document,
        fields: &[crate::core::field::FieldDefinition],
        auth_user: &Option<AuthUser>,
    ) {
        let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
        let conn = match self.pool.get() {
            Ok(c) => c,
            Err(_) => return,
        };
        let denied = self
            .hook_runner
            .check_field_read_access(fields, user_doc, &conn);
        if let Some(ref mut s) = doc.fields {
            for name in &denied {
                s.fields.remove(name);
            }
        }
    }
}

/// Untestable as unit: all methods are async gRPC handlers requiring full server + Lua VM + DB.
/// Covered by integration tests in tests/ directory.
#[cfg(not(tarpaulin_include))]
#[tonic::async_trait]
impl ContentApi for ContentService {
    /// Find documents in a collection with optional filters, sorting, and pagination.
    async fn find(
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

        // Parse filters: prefer `where` JSON field, fall back to legacy `filters` map
        let mut filters = if let Some(ref where_json) = req.r#where {
            parse_where_json(where_json)
                .map_err(|e| Status::invalid_argument(format!("Invalid where clause: {}", e)))?
        } else if !req.filters.is_empty() {
            req.filters
                .iter()
                .map(|(k, v)| {
                    FilterClause::Single(Filter {
                        field: k.clone(),
                        op: FilterOp::Equals(v.clone()),
                    })
                })
                .collect()
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
        let def_owned = def;
        let (documents, total) = tokio::task::spawn_blocking(move || {
            runner.fire_before_read(&hooks, &collection, "find", HashMap::new())?;
            let mut docs = ops::find_documents(
                &pool,
                &collection,
                &def_owned,
                &find_query,
                locale_ctx.as_ref(),
            )?;
            let total = ops::count_documents(
                &pool,
                &collection,
                &def_owned,
                &filters,
                locale_ctx.as_ref(),
            )?;
            // Hydrate join table data (has-many relationships and arrays)
            let conn = pool.get().context("DB connection for hydration")?;
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
            // Populate relationships if depth > 0
            if depth > 0 {
                let reg = registry
                    .read()
                    .map_err(|e| anyhow::anyhow!("Registry lock: {}", e))?;
                let mut docs = docs;
                for doc in &mut docs {
                    let mut visited = std::collections::HashSet::new();
                    query::populate_relationships(
                        &conn,
                        &reg,
                        &collection,
                        &def_owned,
                        doc,
                        depth,
                        &mut visited,
                        select_slice,
                    )?;
                }
                return Ok((docs, total));
            }
            Ok::<_, anyhow::Error>((docs, total))
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| Status::internal(format!("Query error: {}", e)))?;

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
    async fn find_by_id(
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
                    let reg = registry
                        .read()
                        .map_err(|e| anyhow::anyhow!("Registry lock: {}", e))?;
                    let mut visited = std::collections::HashSet::new();
                    query::populate_relationships(
                        &conn,
                        &reg,
                        &collection,
                        &def_owned,
                        d,
                        depth,
                        &mut visited,
                        select_slice,
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
        .map_err(|e| Status::internal(format!("Query error: {}", e)))?;

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
    async fn create(
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

        // Strip field-level create-denied fields
        {
            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let conn = self
                .pool
                .get()
                .map_err(|_| Status::internal("Database connection error"))?;
            let denied =
                self.hook_runner
                    .check_field_write_access(&def.fields, user_doc, "create", &conn);
            for name in &denied {
                data.remove(name);
            }
        }

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
        .map_err(|e| Status::internal(format!("Create error: {}", e)))?;

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
    async fn update(
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

        // Strip field-level update-denied fields
        {
            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let conn = self
                .pool
                .get()
                .map_err(|_| Status::internal("Database connection error"))?;
            let denied =
                self.hook_runner
                    .check_field_write_access(&def.fields, user_doc, "update", &conn);
            for name in &denied {
                data.remove(name);
            }
        }

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
            .map_err(|e| Status::internal(format!("Unpublish error: {}", e)))?;

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
        .map_err(|e| Status::internal(format!("Update error: {}", e)))?;

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
    async fn delete(
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
        .map_err(|e| Status::internal(format!("Delete error: {}", e)))?;

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
    async fn count(
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
        } else if !req.filters.is_empty() {
            req.filters
                .iter()
                .map(|(k, v)| {
                    FilterClause::Single(Filter {
                        field: k.clone(),
                        op: FilterOp::Equals(v.clone()),
                    })
                })
                .collect()
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
        .map_err(|e| Status::internal(format!("Count error: {}", e)))?;

        Ok(Response::new(content::CountResponse { count }))
    }

    /// Bulk update matching documents (all-or-nothing access check, no per-document hooks).
    async fn update_many(
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
        } else if !req.filters.is_empty() {
            req.filters
                .iter()
                .map(|(k, v)| {
                    FilterClause::Single(Filter {
                        field: k.clone(),
                        op: FilterOp::Equals(v.clone()),
                    })
                })
                .collect()
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
            let tx = conn.transaction().context("Start transaction")?;

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
                Status::internal(format!("UpdateMany error: {}", e))
            }
        })?;

        Ok(Response::new(content::UpdateManyResponse { modified }))
    }

    /// Bulk delete matching documents (all-or-nothing access check, no per-document hooks).
    async fn delete_many(
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
        } else if !req.filters.is_empty() {
            req.filters
                .iter()
                .map(|(k, v)| {
                    FilterClause::Single(Filter {
                        field: k.clone(),
                        op: FilterOp::Equals(v.clone()),
                    })
                })
                .collect()
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
            let tx = conn.transaction().context("Start transaction")?;

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
                Status::internal(format!("DeleteMany error: {}", e))
            }
        })?;

        Ok(Response::new(content::DeleteManyResponse { deleted }))
    }

    /// Get the single document for a global definition.
    async fn get_global(
        &self,
        request: Request<content::GetGlobalRequest>,
    ) -> Result<Response<content::GetGlobalResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_global_def(&req.slug)?;

        // Check read access
        let access_result =
            self.require_access(def.access.read.as_deref(), &auth_user, None, None)?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Read access denied"));
        }

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let hooks = def.hooks.clone();
        let def_fields = def.fields.clone();
        let fields = def_fields.clone();
        let slug = req.slug.clone();
        let doc = tokio::task::spawn_blocking(move || {
            runner.fire_before_read(&hooks, &slug, "get_global", HashMap::new())?;
            let doc = ops::get_global(&pool, &slug, &def, locale_ctx.as_ref())?;
            let doc = runner.apply_after_read(&hooks, &fields, &slug, "get_global", doc);
            Ok::<_, anyhow::Error>(doc)
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| Status::internal(format!("Query error: {}", e)))?;

        let mut proto_doc = document_to_proto(&doc, &req.slug);
        self.strip_denied_read_fields(&mut proto_doc, &def_fields, &auth_user);

        Ok(Response::new(content::GetGlobalResponse {
            document: Some(proto_doc),
        }))
    }

    /// Update a global's document, running hooks within a transaction.
    async fn update_global(
        &self,
        request: Request<content::UpdateGlobalRequest>,
    ) -> Result<Response<content::UpdateGlobalResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_global_def(&req.slug)?;

        // Check update access
        let access_result =
            self.require_access(def.access.update.as_deref(), &auth_user, None, None)?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Update access denied"));
        }

        // Extract join table data (preserves structured arrays/objects)
        let join_data = req
            .data
            .as_ref()
            .map(prost_struct_to_json_map)
            .unwrap_or_default();

        // Strip field-level update-denied fields
        let mut data = req
            .data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();
        {
            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let conn = self
                .pool
                .get()
                .map_err(|_| Status::internal("Database connection error"))?;
            let denied =
                self.hook_runner
                    .check_field_write_access(&def.fields, user_doc, "update", &conn);
            for name in &denied {
                data.remove(name);
            }
        }

        let locale_ctx =
            LocaleContext::from_locale_string(req.locale.as_deref(), &self.locale_config);

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let slug = req.slug.clone();
        let def_fields = def.fields.clone();
        let def_owned = def;
        let user_doc = auth_user.as_ref().map(|au| au.user_doc.clone());
        let (doc, _req_context) = tokio::task::spawn_blocking(move || {
            crate::service::update_global_document(
                &pool,
                &runner,
                &slug,
                &def_owned,
                data,
                &join_data,
                locale_ctx.as_ref(),
                None,
                user_doc.as_ref(),
                false,
            )
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| Status::internal(format!("Update global error: {}", e)))?;

        {
            let def = self.get_global_def(&req.slug);
            let (hooks, live) = match &def {
                Ok(d) => (d.hooks.clone(), d.live.clone()),
                Err(_) => (Default::default(), None),
            };
            self.hook_runner.publish_event(
                &self.event_bus,
                &hooks,
                live.as_ref(),
                crate::core::event::EventTarget::Global,
                crate::core::event::EventOperation::Update,
                req.slug.clone(),
                doc.id.clone(),
                doc.fields.clone(),
                Self::event_user_from(&auth_user),
            );
        }

        let mut proto_doc = document_to_proto(&doc, &req.slug);
        self.strip_denied_read_fields(&mut proto_doc, &def_fields, &auth_user);

        Ok(Response::new(content::UpdateGlobalResponse {
            document: Some(proto_doc),
        }))
    }

    /// Authenticate with email/password and return a JWT token.
    async fn login(
        &self,
        request: Request<content::LoginRequest>,
    ) -> Result<Response<content::LoginResponse>, Status> {
        self.login_impl(request).await
    }

    /// Initiate a password reset flow.
    async fn forgot_password(
        &self,
        request: Request<content::ForgotPasswordRequest>,
    ) -> Result<Response<content::ForgotPasswordResponse>, Status> {
        self.forgot_password_impl(request).await
    }

    /// Reset a password using a valid reset token.
    async fn reset_password(
        &self,
        request: Request<content::ResetPasswordRequest>,
    ) -> Result<Response<content::ResetPasswordResponse>, Status> {
        self.reset_password_impl(request).await
    }

    /// Verify an email address using a verification token.
    async fn verify_email(
        &self,
        request: Request<content::VerifyEmailRequest>,
    ) -> Result<Response<content::VerifyEmailResponse>, Status> {
        self.verify_email_impl(request).await
    }

    /// List all registered collections and globals.
    async fn list_collections(
        &self,
        _request: Request<content::ListCollectionsRequest>,
    ) -> Result<Response<content::ListCollectionsResponse>, Status> {
        let reg = self
            .registry
            .read()
            .map_err(|e| Status::internal(format!("Registry lock poisoned: {}", e)))?;

        let mut collections: Vec<content::CollectionInfo> = reg
            .collections
            .values()
            .map(|def| content::CollectionInfo {
                slug: def.slug.clone(),
                singular_label: def
                    .labels
                    .singular
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                plural_label: def
                    .labels
                    .plural
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                timestamps: def.timestamps,
                auth: def.is_auth_collection(),
                upload: def.is_upload_collection(),
            })
            .collect();
        collections.sort_by(|a, b| a.slug.cmp(&b.slug));

        let mut globals: Vec<content::GlobalInfo> = reg
            .globals
            .values()
            .map(|def| content::GlobalInfo {
                slug: def.slug.clone(),
                singular_label: def
                    .labels
                    .singular
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                plural_label: def
                    .labels
                    .plural
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
            })
            .collect();
        globals.sort_by(|a, b| a.slug.cmp(&b.slug));

        Ok(Response::new(content::ListCollectionsResponse {
            collections,
            globals,
        }))
    }

    /// Describe a collection's schema (fields, timestamps, auth, upload).
    async fn describe_collection(
        &self,
        request: Request<content::DescribeCollectionRequest>,
    ) -> Result<Response<content::DescribeCollectionResponse>, Status> {
        let req = request.into_inner();

        if req.is_global {
            let def = self.get_global_def(&req.slug)?;
            Ok(Response::new(content::DescribeCollectionResponse {
                slug: def.slug.clone(),
                singular_label: def
                    .labels
                    .singular
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                plural_label: def
                    .labels
                    .plural
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                timestamps: false,
                auth: false,
                fields: def.fields.iter().map(field_def_to_proto).collect(),
                upload: false,
                drafts: false,
            }))
        } else {
            let def = self.get_collection_def(&req.slug)?;
            Ok(Response::new(content::DescribeCollectionResponse {
                slug: def.slug.clone(),
                singular_label: def
                    .labels
                    .singular
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                plural_label: def
                    .labels
                    .plural
                    .as_ref()
                    .map(|ls| ls.resolve_default().to_string()),
                timestamps: def.timestamps,
                auth: def.is_auth_collection(),
                fields: def.fields.iter().map(field_def_to_proto).collect(),
                upload: def.is_upload_collection(),
                drafts: def.has_drafts(),
            }))
        }
    }

    type SubscribeStream =
        Pin<Box<dyn Stream<Item = Result<content::MutationEvent, Status>> + Send>>;

    /// Subscribe to real-time mutation events (server streaming).
    async fn subscribe(
        &self,
        request: Request<content::SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let req = request.into_inner();

        let event_bus = self
            .event_bus
            .as_ref()
            .ok_or_else(|| Status::unavailable("Live updates disabled"))?;

        // Authenticate subscriber
        let auth_user = if !req.token.is_empty() {
            let claims = crate::core::auth::validate_token(&req.token, &self.jwt_secret)
                .map_err(|_| Status::unauthenticated("Invalid or expired token"))?;
            let pool = self.pool.clone();
            let registry = self.registry.clone();
            let locale_config = self.locale_config.clone();
            tokio::task::spawn_blocking(move || {
                crate::admin::server::load_auth_user(&pool, &registry, &claims, &locale_config)
            })
            .await
            .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        } else {
            None
        };

        // Build allowed set: snapshot access at subscribe time
        let mut allowed_collections: HashSet<String> = HashSet::new();
        let mut allowed_globals: HashSet<String> = HashSet::new();
        let requested_ops: HashSet<String> = if req.operations.is_empty() {
            ["create", "update", "delete"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            req.operations.into_iter().collect()
        };

        {
            let reg = self
                .registry
                .read()
                .map_err(|e| Status::internal(format!("Registry lock poisoned: {}", e)))?;

            let user_doc = auth_user.as_ref().map(|u| &u.user_doc);

            // Check collection read access
            let target_collections: Vec<String> = if req.collections.is_empty() {
                reg.collections.keys().cloned().collect()
            } else {
                req.collections
            };

            let conn = self
                .pool
                .get()
                .map_err(|e| Status::internal(format!("DB connection: {}", e)))?;

            for slug in &target_collections {
                if let Some(def) = reg.get_collection(slug) {
                    match self.hook_runner.check_access(
                        def.access.read.as_deref(),
                        user_doc,
                        None,
                        None,
                        &conn,
                    ) {
                        Ok(AccessResult::Allowed) | Ok(AccessResult::Constrained(_)) => {
                            allowed_collections.insert(slug.clone());
                        }
                        _ => {}
                    }
                }
            }

            let target_globals: Vec<String> = if req.globals.is_empty() {
                reg.globals.keys().cloned().collect()
            } else {
                req.globals
            };

            for slug in &target_globals {
                if let Some(def) = reg.get_global(slug) {
                    match self.hook_runner.check_access(
                        def.access.read.as_deref(),
                        user_doc,
                        None,
                        None,
                        &conn,
                    ) {
                        Ok(AccessResult::Allowed) | Ok(AccessResult::Constrained(_)) => {
                            allowed_globals.insert(slug.clone());
                        }
                        _ => {}
                    }
                }
            }
        }

        if allowed_collections.is_empty() && allowed_globals.is_empty() {
            return Err(Status::permission_denied(
                "No accessible collections or globals",
            ));
        }

        let rx = event_bus.subscribe();
        let stream = BroadcastStream::new(rx).filter_map(move |result| {
            match result {
                Ok(event) => {
                    // Filter by target type + collection access
                    let allowed = match event.target {
                        crate::core::event::EventTarget::Collection => {
                            allowed_collections.contains(&event.collection)
                        }
                        crate::core::event::EventTarget::Global => {
                            allowed_globals.contains(&event.collection)
                        }
                    };
                    if !allowed {
                        return None;
                    }

                    // Filter by operation
                    let op_str = match event.operation {
                        crate::core::event::EventOperation::Create => "create",
                        crate::core::event::EventOperation::Update => "update",
                        crate::core::event::EventOperation::Delete => "delete",
                    };
                    if !requested_ops.contains(op_str) {
                        return None;
                    }

                    // Convert data to prost Struct
                    let fields: BTreeMap<String, prost_types::Value> = event
                        .data
                        .iter()
                        .map(|(k, v)| (k.clone(), json_to_prost_value(v)))
                        .collect();

                    let target_str = match event.target {
                        crate::core::event::EventTarget::Collection => "collection",
                        crate::core::event::EventTarget::Global => "global",
                    };

                    Some(Ok(content::MutationEvent {
                        sequence: event.sequence,
                        timestamp: event.timestamp,
                        target: target_str.to_string(),
                        operation: op_str.to_string(),
                        collection: event.collection,
                        document_id: event.document_id,
                        data: Some(prost_types::Struct { fields }),
                    }))
                }
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                    tracing::warn!("Subscribe stream lagged by {} events", n);
                    None
                }
            }
        });

        Ok(Response::new(Box::pin(stream)))
    }

    /// Return the currently authenticated user from a JWT token.
    async fn me(
        &self,
        request: Request<content::MeRequest>,
    ) -> Result<Response<content::MeResponse>, Status> {
        self.me_impl(request).await
    }

    /// List version history for a document.
    async fn list_versions(
        &self,
        request: Request<content::ListVersionsRequest>,
    ) -> Result<Response<content::ListVersionsResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.has_versions() {
            return Err(Status::failed_precondition(format!(
                "Collection '{}' does not have versioning enabled", req.collection
            )));
        }

        // Check read access
        let access_result =
            self.require_access(def.access.read.as_deref(), &auth_user, Some(&req.id), None)?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Read access denied"));
        }

        let pool = self.pool.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let limit = req.limit;
        let versions = tokio::task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;
            query::list_versions(&conn, &collection, &id, limit, None)
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| Status::internal(format!("List versions error: {}", e)))?;

        let proto_versions: Vec<content::VersionInfo> = versions
            .iter()
            .map(|v| content::VersionInfo {
                id: v.id.clone(),
                version: v.version,
                status: v.status.clone(),
                latest: v.latest,
                created_at: v.created_at.clone().unwrap_or_default(),
            })
            .collect();

        Ok(Response::new(content::ListVersionsResponse {
            versions: proto_versions,
        }))
    }

    /// Restore a document to a previous version.
    async fn restore_version(
        &self,
        request: Request<content::RestoreVersionRequest>,
    ) -> Result<Response<content::RestoreVersionResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.has_versions() {
            return Err(Status::failed_precondition(format!(
                "Collection '{}' does not have versioning enabled", req.collection
            )));
        }

        // Check update access
        let access_result = self.require_access(
            def.access.update.as_deref(),
            &auth_user,
            Some(&req.document_id),
            None,
        )?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Update access denied"));
        }

        let pool = self.pool.clone();
        let collection = req.collection.clone();
        let document_id = req.document_id.clone();
        let version_id = req.version_id.clone();
        let def_owned = def.clone();
        let locale_config = self.locale_config.clone();
        let doc = tokio::task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;
            let version = query::find_version_by_id(&conn, &collection, &version_id)?
                .ok_or_else(|| anyhow::anyhow!("Version '{}' not found", version_id))?;
            query::restore_version(
                &conn, &collection, &def_owned, &document_id,
                &version.snapshot, &version.status, &locale_config,
            )
        })
        .await
        .map_err(|e| Status::internal(format!("Task error: {}", e)))?
        .map_err(|e| Status::internal(format!("Restore error: {}", e)))?;

        let proto_doc = document_to_proto(&doc, &req.collection);

        Ok(Response::new(content::RestoreVersionResponse {
            document: Some(proto_doc),
        }))
    }

    /// List all defined jobs and their configuration.
    async fn list_jobs(
        &self,
        request: Request<content::ListJobsRequest>,
    ) -> Result<Response<content::ListJobsResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        if auth_user.is_none() {
            return Err(Status::unauthenticated("Authentication required"));
        }

        let reg = self.registry.read()
            .map_err(|e| Status::internal(format!("Registry lock poisoned: {}", e)))?;

        let jobs: Vec<content::JobDefinitionInfo> = reg.jobs.iter().map(|(slug, def)| {
            content::JobDefinitionInfo {
                slug: slug.clone(),
                handler: def.handler.clone(),
                schedule: def.schedule.clone(),
                queue: def.queue.clone(),
                retries: def.retries,
                timeout: def.timeout,
                concurrency: def.concurrency,
                skip_if_running: def.skip_if_running,
                label: def.labels.singular.clone(),
            }
        }).collect();

        Ok(Response::new(content::ListJobsResponse { jobs }))
    }

    /// Trigger a job by slug, queuing it for execution.
    async fn trigger_job(
        &self,
        request: Request<content::TriggerJobRequest>,
    ) -> Result<Response<content::TriggerJobResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        if auth_user.is_none() {
            return Err(Status::unauthenticated("Authentication required"));
        }
        let req = request.into_inner();

        // Look up job definition
        let job_def = {
            let reg = self.registry.read()
                .map_err(|e| Status::internal(format!("Registry lock poisoned: {}", e)))?;
            reg.get_job(&req.slug).cloned()
                .ok_or_else(|| Status::not_found(format!("Job '{}' not found", req.slug)))?
        };

        // Check access if defined
        if let Some(ref access_ref) = job_def.access {
            let result = self.require_access(Some(access_ref), &auth_user, None, None)?;
            if matches!(result, AccessResult::Denied) {
                return Err(Status::permission_denied("Trigger access denied"));
            }
        }

        let data_json = req.data_json.unwrap_or_else(|| "{}".to_string());
        let conn = self.pool.get()
            .map_err(|_| Status::internal("Database connection error"))?;

        let job_run = crate::db::query::jobs::insert_job(
            &conn,
            &req.slug,
            &data_json,
            "grpc",
            job_def.retries + 1,
            &job_def.queue,
        ).map_err(|e| Status::internal(format!("Failed to queue job: {}", e)))?;

        Ok(Response::new(content::TriggerJobResponse {
            job_id: job_run.id,
        }))
    }

    /// Get details of a specific job run.
    async fn get_job_run(
        &self,
        request: Request<content::GetJobRunRequest>,
    ) -> Result<Response<content::GetJobRunResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        if auth_user.is_none() {
            return Err(Status::unauthenticated("Authentication required"));
        }
        let req = request.into_inner();

        let conn = self.pool.get()
            .map_err(|_| Status::internal("Database connection error"))?;

        let run = crate::db::query::jobs::get_job_run(&conn, &req.id)
            .map_err(|e| Status::internal(format!("Query error: {}", e)))?
            .ok_or_else(|| Status::not_found(format!("Job run '{}' not found", req.id)))?;

        Ok(Response::new(job_run_to_proto(&run)))
    }

    /// List job runs with optional filters.
    async fn list_job_runs(
        &self,
        request: Request<content::ListJobRunsRequest>,
    ) -> Result<Response<content::ListJobRunsResponse>, Status> {
        let metadata = request.metadata().clone();
        let auth_user = self.extract_auth_user(&metadata);
        if auth_user.is_none() {
            return Err(Status::unauthenticated("Authentication required"));
        }
        let req = request.into_inner();

        let conn = self.pool.get()
            .map_err(|_| Status::internal("Database connection error"))?;

        let limit = req.limit.unwrap_or(50);
        let offset = req.offset.unwrap_or(0);

        let runs = crate::db::query::jobs::list_job_runs(
            &conn,
            req.slug.as_deref(),
            req.status.as_deref(),
            limit,
            offset,
        ).map_err(|e| Status::internal(format!("Query error: {}", e)))?;

        let runs: Vec<content::GetJobRunResponse> = runs.iter()
            .map(job_run_to_proto)
            .collect();

        Ok(Response::new(content::ListJobRunsResponse { runs }))
    }
}

/// Convert a JobRun to gRPC response.
#[cfg(not(tarpaulin_include))]
fn job_run_to_proto(run: &crate::core::job::JobRun) -> content::GetJobRunResponse {
    content::GetJobRunResponse {
        id: run.id.clone(),
        slug: run.slug.clone(),
        status: run.status.as_str().to_string(),
        data_json: run.data.clone(),
        result_json: run.result.clone(),
        error: run.error.clone(),
        attempt: run.attempt,
        max_attempts: run.max_attempts,
        scheduled_by: run.scheduled_by.clone(),
        created_at: run.created_at.clone(),
        started_at: run.started_at.clone(),
        completed_at: run.completed_at.clone(),
    }
}
