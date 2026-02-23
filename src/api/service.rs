//! Tonic gRPC service implementing all ContentAPI RPCs.

use anyhow::Context as _;
use std::collections::{BTreeMap, HashMap};
use tonic::{Request, Response, Status};
use tonic::metadata::MetadataMap;

use crate::core::SharedRegistry;
use crate::core::auth::{self, AuthUser};
use crate::core::upload;
use crate::db::DbPool;
use crate::db::{ops, query};
use crate::db::query::{AccessResult, FindQuery, Filter, FilterOp, FilterClause};
use crate::hooks::lifecycle::{self, HookContext, HookEvent, HookRunner};
use super::content;
use super::content::content_api_server::ContentApi;

/// Implements the gRPC ContentAPI service (Find, Create, Update, Delete, Login, etc.).
pub struct ContentService {
    pool: DbPool,
    registry: SharedRegistry,
    hook_runner: HookRunner,
    jwt_secret: String,
    default_depth: i32,
    max_depth: i32,
}

impl ContentService {
    pub fn new(
        pool: DbPool,
        registry: SharedRegistry,
        hook_runner: HookRunner,
        jwt_secret: String,
        depth_config: &crate::config::DepthConfig,
    ) -> Self {
        Self {
            pool,
            registry,
            hook_runner,
            jwt_secret,
            default_depth: depth_config.default_depth,
            max_depth: depth_config.max_depth,
        }
    }

    #[allow(clippy::result_large_err)]
    fn get_collection_def(&self, slug: &str) -> Result<crate::core::CollectionDefinition, Status> {
        let reg = self.registry.read()
            .map_err(|e| Status::internal(format!("Registry lock poisoned: {}", e)))?;
        reg.get_collection(slug)
            .cloned()
            .ok_or_else(|| Status::not_found(format!("Collection '{}' not found", slug)))
    }

    #[allow(clippy::result_large_err)]
    fn get_global_def(&self, slug: &str) -> Result<crate::core::collection::GlobalDefinition, Status> {
        let reg = self.registry.read()
            .map_err(|e| Status::internal(format!("Registry lock poisoned: {}", e)))?;
        reg.get_global(slug)
            .cloned()
            .ok_or_else(|| Status::not_found(format!("Global '{}' not found", slug)))
    }

    /// Extract auth user from gRPC metadata (Bearer token in `authorization` header).
    fn extract_auth_user(&self, metadata: &MetadataMap) -> Option<AuthUser> {
        let token = metadata.get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))?;
        let claims = auth::validate_token(token, &self.jwt_secret).ok()?;
        let def = {
            let reg = self.registry.read().ok()?;
            reg.get_collection(&claims.collection)?.clone()
        };
        let conn = self.pool.get().ok()?;
        let doc = query::find_by_id(&conn, &claims.collection, &def, &claims.sub).ok()??;
        Some(AuthUser { claims, user_doc: doc })
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
        let conn = self.pool.get()
            .map_err(|_| Status::internal("Database connection error"))?;
        self.hook_runner.check_access(access_ref, user_doc, id, data, &conn)
            .map_err(|e| Status::internal(format!("Access check error: {}", e)))
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
        let denied = self.hook_runner.check_field_read_access(fields, user_doc, &conn);
        if let Some(ref mut s) = doc.fields {
            for name in &denied {
                s.fields.remove(name);
            }
        }
    }
}

fn document_to_proto(doc: &crate::core::Document, collection: &str) -> content::Document {
    let mut fields = prost_types::Struct {
        fields: BTreeMap::new(),
    };

    for (k, v) in &doc.fields {
        fields.fields.insert(k.clone(), json_to_prost_value(v));
    }

    content::Document {
        id: doc.id.clone(),
        collection: collection.to_string(),
        fields: Some(fields),
        created_at: doc.created_at.clone(),
        updated_at: doc.updated_at.clone(),
    }
}

fn json_to_prost_value(v: &serde_json::Value) -> prost_types::Value {
    match v {
        serde_json::Value::Null => prost_types::Value {
            kind: Some(prost_types::value::Kind::NullValue(0)),
        },
        serde_json::Value::Bool(b) => prost_types::Value {
            kind: Some(prost_types::value::Kind::BoolValue(*b)),
        },
        serde_json::Value::Number(n) => prost_types::Value {
            kind: Some(prost_types::value::Kind::NumberValue(n.as_f64().unwrap_or(0.0))),
        },
        serde_json::Value::String(s) => prost_types::Value {
            kind: Some(prost_types::value::Kind::StringValue(s.clone())),
        },
        serde_json::Value::Array(arr) => {
            let values: Vec<_> = arr.iter().map(json_to_prost_value).collect();
            prost_types::Value {
                kind: Some(prost_types::value::Kind::ListValue(prost_types::ListValue { values })),
            }
        }
        serde_json::Value::Object(map) => {
            let mut fields = BTreeMap::new();
            for (k, v) in map {
                fields.insert(k.clone(), json_to_prost_value(v));
            }
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StructValue(prost_types::Struct { fields })),
            }
        }
    }
}

fn prost_struct_to_hashmap(s: &prost_types::Struct) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (k, v) in &s.fields {
        let value_str = match &v.kind {
            Some(prost_types::value::Kind::StringValue(s)) => s.clone(),
            Some(prost_types::value::Kind::NumberValue(n)) => n.to_string(),
            Some(prost_types::value::Kind::BoolValue(b)) => b.to_string(),
            Some(prost_types::value::Kind::NullValue(_)) => String::new(),
            _ => String::from("null"),
        };
        map.insert(k.clone(), value_str);
    }
    map
}

/// Convert a prost Struct to a JSON Value map, preserving arrays and nested objects.
/// Used for extracting join table data (has-many relationships and arrays).
fn prost_struct_to_json_map(s: &prost_types::Struct) -> HashMap<String, serde_json::Value> {
    let mut map = HashMap::new();
    for (k, v) in &s.fields {
        map.insert(k.clone(), prost_value_to_json(v));
    }
    map
}

fn prost_value_to_json(v: &prost_types::Value) -> serde_json::Value {
    match &v.kind {
        Some(prost_types::value::Kind::NullValue(_)) => serde_json::Value::Null,
        Some(prost_types::value::Kind::BoolValue(b)) => serde_json::Value::Bool(*b),
        Some(prost_types::value::Kind::NumberValue(n)) => {
            serde_json::Number::from_f64(*n)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        }
        Some(prost_types::value::Kind::StringValue(s)) => serde_json::Value::String(s.clone()),
        Some(prost_types::value::Kind::ListValue(list)) => {
            serde_json::Value::Array(list.values.iter().map(prost_value_to_json).collect())
        }
        Some(prost_types::value::Kind::StructValue(s)) => {
            let obj: serde_json::Map<String, serde_json::Value> = s.fields.iter()
                .map(|(k, v)| (k.clone(), prost_value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        None => serde_json::Value::Null,
    }
}

fn field_def_to_proto(field: &crate::core::field::FieldDefinition) -> content::FieldInfo {
    content::FieldInfo {
        name: field.name.clone(),
        r#type: field.field_type.as_str().to_string(),
        required: field.required,
        unique: field.unique,
        relationship_collection: field.relationship.as_ref().map(|r| r.collection.clone()),
        relationship_has_many: field.relationship.as_ref().map(|r| r.has_many),
        options: field.options.iter().map(|o| content::SelectOptionInfo {
            label: o.label.clone(),
            value: o.value.clone(),
        }).collect(),
        fields: field.fields.iter().map(field_def_to_proto).collect(),
        relationship_max_depth: field.relationship.as_ref().and_then(|r| r.max_depth),
    }
}

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
        let access_result = self.require_access(
            def.access.read.as_deref(), &auth_user, None, None,
        )?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Read access denied"));
        }

        // Parse filters: prefer `where` JSON field, fall back to legacy `filters` map
        let mut filters = if let Some(ref where_json) = req.r#where {
            parse_where_json(where_json)
                .map_err(|e| Status::invalid_argument(format!("Invalid where clause: {}", e)))?
        } else if !req.filters.is_empty() {
            req.filters.iter()
                .map(|(k, v)| FilterClause::Single(Filter {
                    field: k.clone(),
                    op: FilterOp::Equals(v.clone()),
                }))
                .collect()
        } else {
            Vec::new()
        };

        // Merge access constraint filters
        if let AccessResult::Constrained(ref constraint_filters) = access_result {
            filters.extend(constraint_filters.clone());
        }

        let find_query = FindQuery {
            filters: filters.clone(),
            order_by: req.order_by,
            limit: req.limit,
            offset: req.offset,
        };

        // Validate filter/order_by fields early for a clear INVALID_ARGUMENT status
        query::validate_query_fields(&def, &find_query)
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
            let mut docs = ops::find_documents(&pool, &collection, &def_owned, &find_query)?;
            let total = ops::count_documents(&pool, &collection, &def_owned, &filters)?;
            // Hydrate join table data (has-many relationships and arrays)
            let conn = pool.get().context("DB connection for hydration")?;
            for doc in &mut docs {
                query::hydrate_document(&conn, &collection, &def_owned, doc)?;
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
                let reg = registry.read()
                    .map_err(|e| anyhow::anyhow!("Registry lock: {}", e))?;
                let mut docs = docs;
                for doc in &mut docs {
                    let mut visited = std::collections::HashSet::new();
                    query::populate_relationships(
                        &conn, &reg, &collection, &def_owned, doc, depth, &mut visited,
                    )?;
                }
                return Ok((docs, total));
            }
            Ok::<_, anyhow::Error>((docs, total))
        }).await
            .map_err(|e| Status::internal(format!("Task error: {}", e)))?
            .map_err(|e| Status::internal(format!("Query error: {}", e)))?;

        let mut proto_docs: Vec<_> = documents.iter()
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
        let access_result = self.require_access(
            def.access.read.as_deref(), &auth_user, Some(&req.id), None,
        )?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Read access denied"));
        }

        let depth = req.depth.unwrap_or(self.default_depth).max(0).min(self.max_depth);

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
            // If constrained, use find with id filter + constraints instead of find_by_id
            let mut doc = if let Some(constraints) = access_constraints {
                let mut filters = constraints;
                filters.push(FilterClause::Single(Filter {
                    field: "id".to_string(),
                    op: FilterOp::Equals(id.clone()),
                }));
                let query = FindQuery { filters, ..Default::default() };
                let docs = ops::find_documents(&pool, &collection, &def_owned, &query)?;
                docs.into_iter().next()
            } else {
                ops::find_document_by_id(&pool, &collection, &def_owned, &id)?
            };
            // Hydrate join table data (has-many relationships and arrays)
            if let Some(ref mut d) = doc {
                let conn = pool.get().context("DB connection for hydration")?;
                query::hydrate_document(&conn, &collection, &def_owned, d)?;
            }
            // Assemble sizes for upload collections
            if let Some(ref mut d) = doc {
                if let Some(ref upload_config) = def_owned.upload {
                    if upload_config.enabled {
                        upload::assemble_sizes_object(d, upload_config);
                    }
                }
            }
            let mut doc = doc.map(|d| runner.apply_after_read(&hooks, &fields, &collection, "find_by_id", d));
            // Populate relationships if depth > 0
            if depth > 0 {
                if let Some(ref mut d) = doc {
                    let conn = pool.get().context("DB connection for population")?;
                    let reg = registry.read()
                        .map_err(|e| anyhow::anyhow!("Registry lock: {}", e))?;
                    let mut visited = std::collections::HashSet::new();
                    query::populate_relationships(
                        &conn, &reg, &collection, &def_owned, d, depth, &mut visited,
                    )?;
                }
            }
            Ok::<_, anyhow::Error>(doc)
        }).await
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
        let access_result = self.require_access(
            def.access.create.as_deref(), &auth_user, None, None,
        )?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Create access denied"));
        }

        // Extract join table data (preserves structured arrays/objects)
        let join_data = req.data.as_ref()
            .map(|s| prost_struct_to_json_map(s))
            .unwrap_or_default();

        let mut data = req.data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        // Strip field-level create-denied fields
        {
            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let conn = self.pool.get()
                .map_err(|_| Status::internal("Database connection error"))?;
            let denied = self.hook_runner.check_field_write_access(&def.fields, user_doc, "create", &conn);
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

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let hooks = def.hooks.clone();
        let collection = req.collection.clone();
        let is_auth = def.is_auth_collection();
        let def_fields = def.fields.clone();
        let def_owned = def;
        let doc = tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().context("DB connection")?;
            let tx = conn.transaction().context("Start transaction")?;

            let hook_ctx = HookContext {
                collection: collection.clone(),
                operation: "create".to_string(),
                data: data.iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect(),
            };
            let final_ctx = runner.run_before_write(
                &hooks, &def_owned.fields, hook_ctx, &tx, &collection, None,
            )?;
            let final_data = lifecycle::hook_ctx_to_string_map(&final_ctx);
            let doc = query::create(&tx, &collection, &def_owned, &final_data)?;

            // Save join table data (has-many relationships and arrays)
            query::save_join_table_data(&tx, &collection, &def_owned, &doc.id, &join_data)?;

            if is_auth {
                if let Some(ref pw) = password {
                    if !pw.is_empty() {
                        query::update_password(&tx, &collection, &doc.id, pw)?;
                    }
                }
            }

            tx.commit().context("Commit transaction")?;
            Ok::<_, anyhow::Error>(doc)
        }).await
            .map_err(|e| Status::internal(format!("Task error: {}", e)))?
            .map_err(|e| Status::internal(format!("Create error: {}", e)))?;

        {
            let def = self.get_collection_def(&req.collection);
            let (hooks, fields) = match &def {
                Ok(d) => (d.hooks.clone(), d.fields.clone()),
                Err(_) => (Default::default(), Vec::new()),
            };
            self.hook_runner.fire_after_event(
                &hooks, &fields,
                HookEvent::AfterChange,
                req.collection.clone(), "create".to_string(), doc.fields.clone(),
            );
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
            def.access.update.as_deref(), &auth_user, Some(&req.id), None,
        )?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Update access denied"));
        }

        // Extract join table data (preserves structured arrays/objects)
        let join_data = req.data.as_ref()
            .map(|s| prost_struct_to_json_map(s))
            .unwrap_or_default();

        let mut data = req.data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();

        // Strip field-level update-denied fields
        {
            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let conn = self.pool.get()
                .map_err(|_| Status::internal("Database connection error"))?;
            let denied = self.hook_runner.check_field_write_access(&def.fields, user_doc, "update", &conn);
            for name in &denied {
                data.remove(name);
            }
        }

        let password = if def.is_auth_collection() {
            data.remove("password")
        } else {
            None
        };

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let hooks = def.hooks.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let is_auth = def.is_auth_collection();
        let def_fields = def.fields.clone();
        let def_owned = def;
        let doc = tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().context("DB connection")?;
            let tx = conn.transaction().context("Start transaction")?;

            let hook_ctx = HookContext {
                collection: collection.clone(),
                operation: "update".to_string(),
                data: data.iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect(),
            };
            let final_ctx = runner.run_before_write(
                &hooks, &def_owned.fields, hook_ctx, &tx, &collection, Some(&id),
            )?;
            let final_data = lifecycle::hook_ctx_to_string_map(&final_ctx);
            let doc = query::update(&tx, &collection, &def_owned, &id, &final_data)?;

            // Save join table data (has-many relationships and arrays)
            query::save_join_table_data(&tx, &collection, &def_owned, &doc.id, &join_data)?;

            if is_auth {
                if let Some(ref pw) = password {
                    if !pw.is_empty() {
                        query::update_password(&tx, &collection, &doc.id, pw)?;
                    }
                }
            }

            tx.commit().context("Commit transaction")?;
            Ok::<_, anyhow::Error>(doc)
        }).await
            .map_err(|e| Status::internal(format!("Task error: {}", e)))?
            .map_err(|e| Status::internal(format!("Update error: {}", e)))?;

        {
            let def = self.get_collection_def(&req.collection);
            let (hooks, fields) = match &def {
                Ok(d) => (d.hooks.clone(), d.fields.clone()),
                Err(_) => (Default::default(), Vec::new()),
            };
            self.hook_runner.fire_after_event(
                &hooks, &fields,
                HookEvent::AfterChange,
                req.collection.clone(), "update".to_string(), doc.fields.clone(),
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
            def.access.delete.as_deref(), &auth_user, Some(&req.id), None,
        )?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Delete access denied"));
        }

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let hooks = def.hooks.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().context("DB connection")?;
            let tx = conn.transaction().context("Start transaction")?;

            let hook_ctx = HookContext {
                collection: collection.clone(),
                operation: "delete".to_string(),
                data: [("id".to_string(), serde_json::Value::String(id.clone()))].into(),
            };
            runner.run_hooks_with_conn(
                &hooks, HookEvent::BeforeDelete, hook_ctx, &tx,
            )?;
            query::delete(&tx, &collection, &id)?;
            tx.commit().context("Commit transaction")?;
            Ok::<_, anyhow::Error>(())
        }).await
            .map_err(|e| Status::internal(format!("Task error: {}", e)))?
            .map_err(|e| Status::internal(format!("Delete error: {}", e)))?;

        self.hook_runner.fire_after_event(
            &def.hooks, &def.fields, HookEvent::AfterDelete,
            req.collection.clone(), "delete".to_string(),
            [("id".to_string(), serde_json::Value::String(req.id.clone()))].into(),
        );

        Ok(Response::new(content::DeleteResponse {
            success: true,
        }))
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
        let access_result = self.require_access(
            def.access.read.as_deref(), &auth_user, None, None,
        )?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Read access denied"));
        }

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let hooks = def.hooks.clone();
        let def_fields = def.fields.clone();
        let fields = def_fields.clone();
        let slug = req.slug.clone();
        let doc = tokio::task::spawn_blocking(move || {
            runner.fire_before_read(&hooks, &slug, "get_global", HashMap::new())?;
            let doc = ops::get_global(&pool, &slug, &def)?;
            let doc = runner.apply_after_read(&hooks, &fields, &slug, "get_global", doc);
            Ok::<_, anyhow::Error>(doc)
        }).await
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
        let access_result = self.require_access(
            def.access.update.as_deref(), &auth_user, None, None,
        )?;
        if matches!(access_result, AccessResult::Denied) {
            return Err(Status::permission_denied("Update access denied"));
        }

        // Strip field-level update-denied fields
        let mut data = req.data
            .map(|s| prost_struct_to_hashmap(&s))
            .unwrap_or_default();
        {
            let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
            let conn = self.pool.get()
                .map_err(|_| Status::internal("Database connection error"))?;
            let denied = self.hook_runner.check_field_write_access(&def.fields, user_doc, "update", &conn);
            for name in &denied {
                data.remove(name);
            }
        }

        let pool = self.pool.clone();
        let runner = self.hook_runner.clone();
        let hooks = def.hooks.clone();
        let slug = req.slug.clone();
        let def_fields = def.fields.clone();
        let def_owned = def;
        let doc = tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().context("DB connection")?;
            let tx = conn.transaction().context("Start transaction")?;

            let hook_ctx = HookContext {
                collection: slug.clone(),
                operation: "update".to_string(),
                data: data.iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect(),
            };
            let global_table = format!("_global_{}", slug);
            let final_ctx = runner.run_before_write(
                &hooks, &def_owned.fields, hook_ctx, &tx, &global_table, Some("default"),
            )?;
            let final_data = lifecycle::hook_ctx_to_string_map(&final_ctx);
            let doc = query::update_global(&tx, &slug, &def_owned, &final_data)?;
            tx.commit().context("Commit transaction")?;
            Ok::<_, anyhow::Error>(doc)
        }).await
            .map_err(|e| Status::internal(format!("Task error: {}", e)))?
            .map_err(|e| Status::internal(format!("Update global error: {}", e)))?;

        {
            let def = self.get_global_def(&req.slug);
            let (hooks, fields) = match &def {
                Ok(d) => (d.hooks.clone(), d.fields.clone()),
                Err(_) => (Default::default(), Vec::new()),
            };
            self.hook_runner.fire_after_event(
                &hooks, &fields,
                HookEvent::AfterChange,
                req.slug.clone(), "update".to_string(), doc.fields.clone(),
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
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.is_auth_collection() {
            return Err(Status::invalid_argument(format!(
                "Collection '{}' is not an auth collection", req.collection
            )));
        }

        let pool = self.pool.clone();
        let slug = req.collection.clone();
        let email = req.email.clone();
        let password = req.password.clone();
        let def_owned = def.clone();

        let user = tokio::task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;
            let doc = query::find_by_email(&conn, &slug, &def_owned, &email)?;
            let doc = match doc {
                Some(d) => d,
                None => return Ok(None),
            };
            let hash = query::get_password_hash(&conn, &slug, &doc.id)?;
            let hash = match hash {
                Some(h) => h,
                None => return Ok(None),
            };
            if !auth::verify_password(&password, &hash)? {
                return Ok(None);
            }
            Ok::<_, anyhow::Error>(Some(doc))
        }).await
            .map_err(|e| Status::internal(format!("Task error: {}", e)))?
            .map_err(|e| Status::internal(format!("Login error: {}", e)))?;

        let user = user.ok_or_else(|| Status::unauthenticated("Invalid email or password"))?;

        let user_email = user.fields.get("email")
            .and_then(|v| v.as_str())
            .unwrap_or(&req.email)
            .to_string();

        let expiry = def.auth.as_ref()
            .map(|a| a.token_expiry)
            .unwrap_or(7200);

        let claims = auth::Claims {
            sub: user.id.clone(),
            collection: req.collection.clone(),
            email: user_email,
            exp: (chrono::Utc::now().timestamp() as u64) + expiry,
        };

        let token = auth::create_token(&claims, &self.jwt_secret)
            .map_err(|e| Status::internal(format!("Token error: {}", e)))?;

        Ok(Response::new(content::LoginResponse {
            token,
            user: Some(document_to_proto(&user, &req.collection)),
        }))
    }

    /// List all registered collections and globals.
    async fn list_collections(
        &self,
        _request: Request<content::ListCollectionsRequest>,
    ) -> Result<Response<content::ListCollectionsResponse>, Status> {
        let reg = self.registry.read()
            .map_err(|e| Status::internal(format!("Registry lock poisoned: {}", e)))?;

        let mut collections: Vec<content::CollectionInfo> = reg.collections.values()
            .map(|def| content::CollectionInfo {
                slug: def.slug.clone(),
                singular_label: def.labels.singular.clone(),
                plural_label: def.labels.plural.clone(),
                timestamps: def.timestamps,
                auth: def.is_auth_collection(),
                upload: def.is_upload_collection(),
            })
            .collect();
        collections.sort_by(|a, b| a.slug.cmp(&b.slug));

        let mut globals: Vec<content::GlobalInfo> = reg.globals.values()
            .map(|def| content::GlobalInfo {
                slug: def.slug.clone(),
                singular_label: def.labels.singular.clone(),
                plural_label: def.labels.plural.clone(),
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
                singular_label: def.labels.singular.clone(),
                plural_label: def.labels.plural.clone(),
                timestamps: false,
                auth: false,
                fields: def.fields.iter().map(field_def_to_proto).collect(),
                upload: false,
            }))
        } else {
            let def = self.get_collection_def(&req.slug)?;
            Ok(Response::new(content::DescribeCollectionResponse {
                slug: def.slug.clone(),
                singular_label: def.labels.singular.clone(),
                plural_label: def.labels.plural.clone(),
                timestamps: def.timestamps,
                auth: def.is_auth_collection(),
                fields: def.fields.iter().map(field_def_to_proto).collect(),
                upload: def.is_upload_collection(),
            }))
        }
    }

    /// Return the currently authenticated user from a JWT token.
    async fn me(
        &self,
        request: Request<content::MeRequest>,
    ) -> Result<Response<content::MeResponse>, Status> {
        let req = request.into_inner();

        let claims = auth::validate_token(&req.token, &self.jwt_secret)
            .map_err(|_| Status::unauthenticated("Invalid or expired token"))?;

        let def = self.get_collection_def(&claims.collection)?;

        let pool = self.pool.clone();
        let collection = claims.collection.clone();
        let id = claims.sub.clone();
        let doc = tokio::task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;
            query::find_by_id(&conn, &collection, &def, &id)
        }).await
            .map_err(|e| Status::internal(format!("Task error: {}", e)))?
            .map_err(|e| Status::internal(format!("Query error: {}", e)))?;

        let doc = doc.ok_or_else(|| Status::not_found("User not found"))?;

        Ok(Response::new(content::MeResponse {
            user: Some(document_to_proto(&doc, &claims.collection)),
        }))
    }
}

/// Parse a JSON `where` clause into `Vec<FilterClause>`.
/// Format: `{ "field": { "op": "value" }, "field2": "simple_value" }`
fn parse_where_json(json_str: &str) -> Result<Vec<FilterClause>, String> {
    let obj: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| format!("JSON parse error: {}", e))?;

    let map = obj.as_object()
        .ok_or_else(|| "where clause must be a JSON object".to_string())?;

    let mut clauses = Vec::new();
    for (field, value) in map {
        match value {
            serde_json::Value::String(s) => {
                clauses.push(FilterClause::Single(Filter {
                    field: field.clone(),
                    op: FilterOp::Equals(s.clone()),
                }));
            }
            serde_json::Value::Object(ops) => {
                for (op_name, op_value) in ops {
                    let op = parse_filter_op(op_name, op_value)
                        .map_err(|e| format!("field '{}': {}", field, e))?;
                    clauses.push(FilterClause::Single(Filter {
                        field: field.clone(),
                        op,
                    }));
                }
            }
            _ => return Err(format!("field '{}': value must be string or operator object", field)),
        }
    }
    Ok(clauses)
}

fn parse_filter_op(op_name: &str, value: &serde_json::Value) -> Result<FilterOp, String> {
    match op_name {
        "equals" => Ok(FilterOp::Equals(value_to_string(value)?)),
        "not_equals" => Ok(FilterOp::NotEquals(value_to_string(value)?)),
        "like" => Ok(FilterOp::Like(value_to_string(value)?)),
        "contains" => Ok(FilterOp::Contains(value_to_string(value)?)),
        "greater_than" => Ok(FilterOp::GreaterThan(value_to_string(value)?)),
        "less_than" => Ok(FilterOp::LessThan(value_to_string(value)?)),
        "greater_than_or_equal" => Ok(FilterOp::GreaterThanOrEqual(value_to_string(value)?)),
        "less_than_or_equal" => Ok(FilterOp::LessThanOrEqual(value_to_string(value)?)),
        "in" => {
            let arr = value.as_array()
                .ok_or_else(|| "'in' operator requires an array".to_string())?;
            let vals: Result<Vec<String>, String> = arr.iter().map(value_to_string).collect();
            Ok(FilterOp::In(vals?))
        }
        "not_in" => {
            let arr = value.as_array()
                .ok_or_else(|| "'not_in' operator requires an array".to_string())?;
            let vals: Result<Vec<String>, String> = arr.iter().map(value_to_string).collect();
            Ok(FilterOp::NotIn(vals?))
        }
        "exists" => Ok(FilterOp::Exists),
        "not_exists" => Ok(FilterOp::NotExists),
        _ => Err(format!("unknown operator '{}'", op_name)),
    }
}

fn value_to_string(v: &serde_json::Value) -> Result<String, String> {
    match v {
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::Bool(b) => Ok(b.to_string()),
        _ => Err("value must be string, number, or boolean".to_string()),
    }
}
