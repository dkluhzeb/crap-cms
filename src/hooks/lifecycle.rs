//! Hook execution engine: runs field, collection, and registered hooks within transactions.

use anyhow::{Context, Result};
use mlua::{Lua, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::config::CrapConfig;
use crate::core::collection::CollectionHooks;
use crate::core::Document;
use crate::core::SharedRegistry;
use crate::core::field::{FieldDefinition, FieldHooks, FieldType};
use crate::core::validate::{FieldError, ValidationError};
use crate::db::query::{self, AccessResult, FindQuery, Filter, FilterOp, FilterClause};

/// Events that trigger hooks.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum HookEvent {
    BeforeValidate,
    BeforeChange,
    AfterChange,
    BeforeRead,
    AfterRead,
    BeforeDelete,
    AfterDelete,
}

impl HookEvent {
    /// Return the Lua event name string for looking up registered hooks.
    pub fn as_str(&self) -> &'static str {
        match self {
            HookEvent::BeforeValidate => "before_validate",
            HookEvent::BeforeChange => "before_change",
            HookEvent::AfterChange => "after_change",
            HookEvent::BeforeRead => "before_read",
            HookEvent::AfterRead => "after_read",
            HookEvent::BeforeDelete => "before_delete",
            HookEvent::AfterDelete => "after_delete",
        }
    }
}

/// Events that trigger field-level hooks.
#[derive(Debug, Clone)]
pub enum FieldHookEvent {
    BeforeValidate,
    BeforeChange,
    AfterChange,
    AfterRead,
}

/// Context passed to hook functions.
#[derive(Debug, Clone)]
pub struct HookContext {
    pub collection: String,
    pub operation: String,
    pub data: HashMap<String, serde_json::Value>,
}

/// Raw pointer wrapper for injecting a transaction/connection into Lua CRUD
/// functions via `lua.set_app_data()`. Only valid between `set_app_data` and
/// `remove_app_data` calls in `run_hooks_with_conn`.
struct TxContext(*const rusqlite::Connection);

// Safety: TxContext is only stored in Lua app_data while the originating
// Connection/Transaction is alive and the Lua mutex is held. The pointer
// is never sent across threads independently.
unsafe impl Send for TxContext {}
unsafe impl Sync for TxContext {}

/// Thread-safe hook runner wrapping a Lua VM.
#[derive(Clone)]
pub struct HookRunner {
    lua: Arc<Mutex<Lua>>,
}

impl HookRunner {
    /// Create a new HookRunner with its own Lua VM, registering CRUD functions and loading init.lua.
    pub fn new(config_dir: &Path, registry: SharedRegistry, config: &CrapConfig) -> Result<Self> {
        let lua = Lua::new();

        // Set up package paths
        let config_str = config_dir.to_string_lossy();
        let code = format!(
            r#"
            package.path = "{0}/?.lua;{0}/?/init.lua;" .. package.path
            package.cpath = "{0}/?.so;{0}/?.dll;" .. package.cpath
            "#,
            config_str
        );
        lua.load(&code).exec().context("Failed to set package paths")?;

        // Register crap.log, crap.util, crap.collections.define, etc.
        super::api::register_api(&lua, registry.clone(), config_dir, config)?;

        // Register CRUD functions on crap.collections (find, find_by_id, create, update, delete).
        // These read the active transaction from Lua app_data when called inside hooks.
        register_crud_functions(&lua, registry)?;

        // Auto-load collections/*.lua and globals/*.lua
        let collections_dir = config_dir.join("collections");
        if collections_dir.exists() {
            super::load_lua_dir(&lua, &collections_dir, "collection")?;
        }
        let globals_dir = config_dir.join("globals");
        if globals_dir.exists() {
            super::load_lua_dir(&lua, &globals_dir, "global")?;
        }

        // Execute init.lua so crap.hooks.register() calls take effect in this VM
        let init_path = config_dir.join("init.lua");
        if init_path.exists() {
            tracing::info!("HookRunner: executing init.lua");
            let code = std::fs::read_to_string(&init_path)
                .with_context(|| format!("Failed to read {}", init_path.display()))?;
            lua.load(&code)
                .set_name(init_path.to_string_lossy())
                .exec()
                .with_context(|| "HookRunner: failed to execute init.lua")?;
        }

        Ok(Self {
            lua: Arc::new(Mutex::new(lua)),
        })
    }

    /// Run all hooks for a given event, mutating the context.
    /// Runs collection-level hook refs first, then global registered hooks.
    /// Does NOT provide CRUD access to hooks (use `run_hooks_with_conn` for that).
    pub fn run_hooks(
        &self,
        hooks: &CollectionHooks,
        event: HookEvent,
        mut context: HookContext,
    ) -> Result<HookContext> {
        let hook_refs = get_hook_refs(hooks, &event);

        let lua = self.lua.lock()
            .map_err(|e| anyhow::anyhow!("Lua VM lock poisoned: {}", e))?;

        for hook_ref in hook_refs {
            tracing::debug!("Running hook: {} for {}", hook_ref, context.collection);
            context = call_hook_ref(&lua, hook_ref, context)?;
        }

        // Run global registered hooks
        context = call_registered_hooks(&lua, &event, context)?;

        Ok(context)
    }

    /// Run hooks with an active database connection/transaction injected.
    /// Runs collection-level hook refs first, then global registered hooks.
    /// CRUD functions (`crap.collections.find`, `.create`, etc.) become available
    /// to Lua hooks and share the provided connection for transaction atomicity.
    pub fn run_hooks_with_conn(
        &self,
        hooks: &CollectionHooks,
        event: HookEvent,
        mut context: HookContext,
        conn: &rusqlite::Connection,
    ) -> Result<HookContext> {
        let hook_refs = get_hook_refs(hooks, &event);

        let lua = self.lua.lock()
            .map_err(|e| anyhow::anyhow!("Lua VM lock poisoned: {}", e))?;

        // Inject the connection pointer so CRUD functions can use it.
        // Safety: conn is valid for the duration of this method, and we hold
        // the Lua mutex so no concurrent access is possible.
        lua.set_app_data(TxContext(conn as *const _));

        let result = (|| -> Result<HookContext> {
            for hook_ref in hook_refs {
                tracing::debug!("Running hook (tx): {} for {}", hook_ref, context.collection);
                context = call_hook_ref(&lua, hook_ref, context)?;
            }
            // Run global registered hooks (with CRUD access via TxContext)
            context = call_registered_hooks(&lua, &event, context)?;
            Ok(context)
        })();

        // Always clean up the connection pointer, even on error.
        lua.remove_app_data::<TxContext>();

        result
    }

    /// Run arbitrary hook refs with an active database connection injected.
    /// Used for system-level hooks like `on_init` that aren't tied to a collection.
    pub fn run_system_hooks_with_conn(
        &self,
        refs: &[String],
        conn: &rusqlite::Connection,
    ) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }

        let lua = self.lua.lock()
            .map_err(|e| anyhow::anyhow!("Lua VM lock poisoned: {}", e))?;

        lua.set_app_data(TxContext(conn as *const _));

        let result = (|| -> Result<()> {
            for hook_ref in refs {
                tracing::debug!("Running system hook: {}", hook_ref);
                let ctx = HookContext {
                    collection: String::new(),
                    operation: "init".to_string(),
                    data: HashMap::new(),
                };
                call_hook_ref(&lua, hook_ref, ctx)?;
            }
            Ok(())
        })();

        lua.remove_app_data::<TxContext>();

        result
    }

    /// Run field-level hooks for a given event, mutating field values in-place.
    /// No CRUD/transaction access — use `run_field_hooks_with_conn` for before-write hooks.
    /// Each hook receives `(value, context)` and returns the new value.
    pub fn run_field_hooks(
        &self,
        fields: &[FieldDefinition],
        event: FieldHookEvent,
        data: &mut HashMap<String, serde_json::Value>,
        collection: &str,
        operation: &str,
    ) -> Result<()> {
        let lua = self.lua.lock()
            .map_err(|e| anyhow::anyhow!("Lua VM lock poisoned: {}", e))?;

        run_field_hooks_inner(&lua, fields, &event, data, collection, operation)
    }

    /// Run field-level hooks with an active database connection/transaction injected.
    /// CRUD functions (`crap.collections.find`, `.create`, etc.) become available
    /// to Lua field hooks, sharing the provided connection for transaction atomicity.
    pub fn run_field_hooks_with_conn(
        &self,
        fields: &[FieldDefinition],
        event: FieldHookEvent,
        data: &mut HashMap<String, serde_json::Value>,
        collection: &str,
        operation: &str,
        conn: &rusqlite::Connection,
    ) -> Result<()> {
        let lua = self.lua.lock()
            .map_err(|e| anyhow::anyhow!("Lua VM lock poisoned: {}", e))?;

        // Inject the connection pointer so CRUD functions can use it.
        lua.set_app_data(TxContext(conn as *const _));

        let result = run_field_hooks_inner(&lua, fields, &event, data, collection, operation);

        // Always clean up, even on error.
        lua.remove_app_data::<TxContext>();

        result
    }

    /// Fire before_read hooks. Returns error to abort the read.
    /// Runs collection-level hook refs, then global registered hooks.
    /// No CRUD access — uses `run_hooks` (no connection).
    pub fn fire_before_read(
        &self,
        hooks: &CollectionHooks,
        collection: &str,
        operation: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<()> {
        let ctx = HookContext {
            collection: collection.to_string(),
            operation: operation.to_string(),
            data,
        };
        self.run_hooks(hooks, HookEvent::BeforeRead, ctx)?;
        Ok(())
    }

    /// Fire after_read hooks on a single document. Returns transformed doc.
    /// Field-level after_read hooks run first, then collection-level, then global registered.
    /// On error: logs warning, returns original doc unmodified.
    pub fn apply_after_read(
        &self,
        hooks: &CollectionHooks,
        fields: &[FieldDefinition],
        collection: &str,
        operation: &str,
        doc: Document,
    ) -> Document {
        let has_field_hooks = fields.iter()
            .any(|f| !f.hooks.after_read.is_empty());

        let mut data: HashMap<String, serde_json::Value> = doc.fields.clone();
        data.insert("id".to_string(), serde_json::Value::String(doc.id.clone()));
        if let Some(ref ts) = doc.created_at {
            data.insert("created_at".to_string(), serde_json::Value::String(ts.clone()));
        }
        if let Some(ref ts) = doc.updated_at {
            data.insert("updated_at".to_string(), serde_json::Value::String(ts.clone()));
        }

        // Run field-level after_read hooks first
        if has_field_hooks {
            if let Err(e) = self.run_field_hooks(
                fields, FieldHookEvent::AfterRead,
                &mut data, collection, operation,
            ) {
                tracing::warn!("field after_read hook error for {}: {}", collection, e);
                return doc;
            }
        }

        let ctx = HookContext {
            collection: collection.to_string(),
            operation: operation.to_string(),
            data,
        };

        // run_hooks handles both collection-level hook refs and global registered hooks
        match self.run_hooks(hooks, HookEvent::AfterRead, ctx) {
            Ok(result_ctx) => {
                let mut fields = result_ctx.data;
                // Extract system fields back out
                fields.remove("id");
                let created_at = fields.remove("created_at")
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .or(doc.created_at.clone());
                let updated_at = fields.remove("updated_at")
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .or(doc.updated_at.clone());

                Document {
                    id: doc.id,
                    fields,
                    created_at,
                    updated_at,
                }
            }
            Err(e) => {
                tracing::warn!("after_read hook error for {}: {}", collection, e);
                doc
            }
        }
    }

    /// Fire after_read hooks on a list of documents.
    pub fn apply_after_read_many(
        &self,
        hooks: &CollectionHooks,
        fields: &[FieldDefinition],
        collection: &str,
        operation: &str,
        docs: Vec<Document>,
    ) -> Vec<Document> {
        let has_field_hooks = fields.iter()
            .any(|f| !f.hooks.after_read.is_empty());

        if !has_field_hooks && hooks.after_read.is_empty() {
            return docs;
        }

        docs.into_iter()
            .map(|doc| self.apply_after_read(hooks, fields, collection, operation, doc))
            .collect()
    }

    /// Run the full before-write lifecycle:
    ///   field BeforeValidate → collection BeforeValidate → validate_fields →
    ///   field BeforeChange → collection BeforeChange.
    /// Returns the final hook context with validated, hook-processed data.
    /// Callers use `hook_ctx_to_string_map()` on the result to get the data for query functions.
    ///
    /// Field hooks in before-write get full CRUD access (same transaction).
    pub fn run_before_write(
        &self,
        hooks: &CollectionHooks,
        fields: &[FieldDefinition],
        mut ctx: HookContext,
        conn: &rusqlite::Connection,
        table: &str,
        exclude_id: Option<&str>,
    ) -> Result<HookContext> {
        // Field-level before_validate (normalize inputs, CRUD available)
        self.run_field_hooks_with_conn(
            fields, FieldHookEvent::BeforeValidate,
            &mut ctx.data, &ctx.collection, &ctx.operation, conn,
        )?;
        // Collection-level before_validate
        let ctx = self.run_hooks_with_conn(hooks, HookEvent::BeforeValidate, ctx, conn)?;
        // Validation
        self.validate_fields(fields, &ctx.data, conn, table, exclude_id)?;
        // Field-level before_change (post-validation transforms, CRUD available)
        let mut ctx = ctx;
        self.run_field_hooks_with_conn(
            fields, FieldHookEvent::BeforeChange,
            &mut ctx.data, &ctx.collection, &ctx.operation, conn,
        )?;
        // Collection-level before_change
        self.run_hooks_with_conn(hooks, HookEvent::BeforeChange, ctx, conn)
    }

    /// Fire an after-event hook in the background (non-blocking, no transaction).
    /// For AfterChange events, field-level after_change hooks run first.
    pub fn fire_after_event(
        &self,
        hooks: &CollectionHooks,
        fields: &[FieldDefinition],
        event: HookEvent,
        collection: String,
        operation: String,
        data: HashMap<String, serde_json::Value>,
    ) {
        let runner = self.clone();
        let hooks = hooks.clone();
        let fields = fields.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut data = data;
            // Run field-level after_change hooks before collection-level
            if matches!(event, HookEvent::AfterChange) {
                let has_field_hooks = fields.iter()
                    .any(|f| !f.hooks.after_change.is_empty());
                if has_field_hooks {
                    if let Err(e) = runner.run_field_hooks(
                        &fields, FieldHookEvent::AfterChange,
                        &mut data, &collection, &operation,
                    ) {
                        tracing::warn!("field after_change hook error for {}: {}", collection, e);
                    }
                }
            }
            let ctx = HookContext {
                collection,
                operation,
                data,
            };
            let _ = runner.run_hooks(&hooks, event, ctx);
        });
    }

    /// Run a custom auth strategy function. Takes a strategy function ref and
    /// a headers map, returns Some(Document) if the strategy authenticates a user.
    /// The strategy function gets CRUD access via the provided connection.
    pub fn run_auth_strategy(
        &self,
        authenticate_ref: &str,
        collection: &str,
        headers: &HashMap<String, String>,
        conn: &rusqlite::Connection,
    ) -> Result<Option<Document>> {
        let lua = self.lua.lock()
            .map_err(|e| anyhow::anyhow!("Lua VM lock poisoned: {}", e))?;

        // Inject connection for CRUD access
        lua.set_app_data(TxContext(conn as *const _));

        let result = (|| -> Result<Option<Document>> {
            // Resolve the function ref
            let parts: Vec<&str> = authenticate_ref.split('.').collect();
            if parts.len() < 2 {
                return Err(anyhow::anyhow!(
                    "Auth strategy ref '{}' must be module.function format", authenticate_ref
                ));
            }
            let module_path = parts[..parts.len() - 1].join(".");
            let func_name = parts[parts.len() - 1];

            let require: mlua::Function = lua.globals().get("require")?;
            let module: mlua::Table = require.call(module_path.clone())
                .with_context(|| format!("Failed to require module '{}'", module_path))?;
            let func: mlua::Function = module.get(func_name)
                .with_context(|| format!("Function '{}' not found in module '{}'", func_name, module_path))?;

            // Build context table: { headers = {...}, collection = "..." }
            let ctx_table = lua.create_table()?;
            let headers_table = lua.create_table()?;
            for (k, v) in headers {
                headers_table.set(k.as_str(), v.as_str())?;
            }
            ctx_table.set("headers", headers_table)?;
            ctx_table.set("collection", collection)?;

            let result: Value = func.call(ctx_table)?;

            match result {
                Value::Table(tbl) => {
                    // Strategy returned a user table — convert to Document
                    let id: String = tbl.get("id")?;
                    let mut fields = HashMap::new();
                    for pair in tbl.pairs::<String, Value>() {
                        let (k, v) = pair?;
                        if k == "id" || k == "created_at" || k == "updated_at" {
                            continue;
                        }
                        fields.insert(k, super::api::lua_to_json(&lua, &v)?);
                    }
                    let created_at: Option<String> = tbl.get("created_at").ok();
                    let updated_at: Option<String> = tbl.get("updated_at").ok();
                    Ok(Some(Document {
                        id,
                        fields,
                        created_at,
                        updated_at,
                    }))
                }
                Value::Nil | Value::Boolean(false) => Ok(None),
                _ => Ok(None),
            }
        })();

        lua.remove_app_data::<TxContext>();
        result
    }

    /// Run a collection-level or global-level access check.
    ///
    /// `access_ref` is the Lua function ref (e.g., "hooks.access.admin_only").
    /// If `None`, access is allowed (no restriction configured).
    /// The function receives `{ user = ..., id = ..., data = ... }` and returns:
    /// - `true` → Allowed
    /// - `false` / `nil` → Denied
    /// - `table` → Constrained (read only: additional WHERE filters)
    pub fn check_access(
        &self,
        access_ref: Option<&str>,
        user: Option<&Document>,
        id: Option<&str>,
        data: Option<&HashMap<String, serde_json::Value>>,
        conn: &rusqlite::Connection,
    ) -> Result<AccessResult> {
        let func_ref = match access_ref {
            Some(r) => r,
            None => return Ok(AccessResult::Allowed),
        };

        let lua = self.lua.lock()
            .map_err(|e| anyhow::anyhow!("Lua VM lock poisoned: {}", e))?;

        // Inject connection for CRUD access in access functions
        lua.set_app_data(TxContext(conn as *const _));

        let result = (|| -> Result<AccessResult> {
            let parts: Vec<&str> = func_ref.split('.').collect();
            if parts.len() < 2 {
                return Err(anyhow::anyhow!(
                    "Access ref '{}' must be module.function format", func_ref
                ));
            }
            let module_path = parts[..parts.len() - 1].join(".");
            let func_name = parts[parts.len() - 1];

            let require: mlua::Function = lua.globals().get("require")?;
            let module: mlua::Table = require.call(module_path.clone())
                .with_context(|| format!("Failed to require module '{}'", module_path))?;
            let func: mlua::Function = module.get(func_name)
                .with_context(|| format!("Function '{}' not found in module '{}'", func_name, module_path))?;

            // Build context table: { user = ..., id = ..., data = ... }
            let ctx_table = lua.create_table()?;
            if let Some(user_doc) = user {
                let user_table = document_to_lua_table(&lua, user_doc)?;
                ctx_table.set("user", user_table)?;
            }
            if let Some(doc_id) = id {
                ctx_table.set("id", doc_id)?;
            }
            if let Some(doc_data) = data {
                let data_table = lua.create_table()?;
                for (k, v) in doc_data {
                    data_table.set(k.as_str(), super::api::json_to_lua(&lua, v)?)?;
                }
                ctx_table.set("data", data_table)?;
            }

            let result: Value = func.call(ctx_table)?;

            match result {
                Value::Boolean(true) => Ok(AccessResult::Allowed),
                Value::Boolean(false) | Value::Nil => Ok(AccessResult::Denied),
                Value::Table(tbl) => {
                    // Parse as filter constraints (same format as find query filters)
                    let mut clauses = Vec::new();
                    for pair in tbl.pairs::<String, Value>() {
                        let (field, value) = pair?;
                        match value {
                            Value::String(s) => {
                                clauses.push(FilterClause::Single(Filter {
                                    field,
                                    op: FilterOp::Equals(s.to_str()?.to_string()),
                                }));
                            }
                            Value::Integer(i) => {
                                clauses.push(FilterClause::Single(Filter {
                                    field,
                                    op: FilterOp::Equals(i.to_string()),
                                }));
                            }
                            Value::Number(n) => {
                                clauses.push(FilterClause::Single(Filter {
                                    field,
                                    op: FilterOp::Equals(n.to_string()),
                                }));
                            }
                            Value::Table(op_tbl) => {
                                for op_pair in op_tbl.pairs::<String, Value>() {
                                    let (op_name, op_val) = op_pair?;
                                    let op = lua_parse_filter_op(&op_name, &op_val)?;
                                    clauses.push(FilterClause::Single(Filter {
                                        field: field.clone(),
                                        op,
                                    }));
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok(AccessResult::Constrained(clauses))
                }
                _ => Ok(AccessResult::Denied),
            }
        })();

        lua.remove_app_data::<TxContext>();
        result
    }

    /// Check field-level read access. Returns a list of field names that should be
    /// stripped from the response (denied fields).
    pub fn check_field_read_access(
        &self,
        fields: &[FieldDefinition],
        user: Option<&Document>,
        conn: &rusqlite::Connection,
    ) -> Vec<String> {
        let mut denied = Vec::new();
        for field in fields {
            if let Some(ref read_ref) = field.access.read {
                match self.check_access(Some(read_ref), user, None, None, conn) {
                    Ok(AccessResult::Allowed) | Ok(AccessResult::Constrained(_)) => {}
                    Ok(AccessResult::Denied) => denied.push(field.name.clone()),
                    Err(e) => {
                        tracing::warn!("field access check error for {}: {}", field.name, e);
                        denied.push(field.name.clone());
                    }
                }
            }
        }
        denied
    }

    /// Check field-level write access for a given operation ("create" or "update").
    /// Returns a list of field names that should be stripped from the input.
    pub fn check_field_write_access(
        &self,
        fields: &[FieldDefinition],
        user: Option<&Document>,
        operation: &str,
        conn: &rusqlite::Connection,
    ) -> Vec<String> {
        let mut denied = Vec::new();
        for field in fields {
            let access_ref = match operation {
                "create" => field.access.create.as_deref(),
                "update" => field.access.update.as_deref(),
                _ => None,
            };
            if let Some(ref_str) = access_ref {
                match self.check_access(Some(ref_str), user, None, None, conn) {
                    Ok(AccessResult::Allowed) | Ok(AccessResult::Constrained(_)) => {}
                    Ok(AccessResult::Denied) => denied.push(field.name.clone()),
                    Err(e) => {
                        tracing::warn!("field write access check error for {}: {}", field.name, e);
                        denied.push(field.name.clone());
                    }
                }
            }
        }
        denied
    }

    /// Validate field data against field definitions.
    /// Checks `required`, `unique`, and custom `validate` (Lua function ref).
    /// Runs inside the caller's transaction for unique checks.
    pub fn validate_fields(
        &self,
        fields: &[FieldDefinition],
        data: &HashMap<String, serde_json::Value>,
        conn: &rusqlite::Connection,
        table: &str,
        exclude_id: Option<&str>,
    ) -> Result<(), ValidationError> {
        let mut errors = Vec::new();

        for field in fields {
            let value = data.get(&field.name);
            let is_empty = match value {
                None => true,
                Some(serde_json::Value::Null) => true,
                Some(serde_json::Value::String(s)) => s.is_empty(),
                _ => false,
            };

            // Required check (skip for checkboxes — absent = false is valid)
            // For Array and has-many Relationship, "required" means at least one item
            if field.required && field.field_type != FieldType::Checkbox {
                if !field.has_parent_column() {
                    // Join-table fields: check for non-empty array in data
                    let has_items = match value {
                        Some(serde_json::Value::Array(arr)) => !arr.is_empty(),
                        Some(serde_json::Value::String(s)) => !s.is_empty(),
                        _ => false,
                    };
                    if !has_items {
                        errors.push(FieldError {
                            field: field.name.clone(),
                            message: format!("{} is required", field.name),
                        });
                    }
                } else if is_empty {
                    errors.push(FieldError {
                        field: field.name.clone(),
                        message: format!("{} is required", field.name),
                    });
                }
            }

            // Unique check (only if value is non-empty, skip for join-table fields)
            if field.unique && !is_empty && field.has_parent_column() {
                let value_str = match value {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                match query::count_where_field_eq(conn, table, &field.name, &value_str, exclude_id) {
                    Ok(count) if count > 0 => {
                        errors.push(FieldError {
                            field: field.name.clone(),
                            message: format!("{} must be unique", field.name),
                        });
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("Unique check failed for {}.{}: {}", table, field.name, e);
                    }
                }
            }

            // Custom validate function (Lua)
            if let Some(ref validate_ref) = field.validate {
                if let Some(val) = value {
                    match self.run_validate_function(validate_ref, val, data, table, &field.name) {
                        Ok(Some(err_msg)) => {
                            errors.push(FieldError {
                                field: field.name.clone(),
                                message: err_msg,
                            });
                        }
                        Ok(None) => {} // valid
                        Err(e) => {
                            tracing::warn!("Validate function '{}' error: {}", validate_ref, e);
                        }
                    }
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ValidationError { errors })
        }
    }

    /// Call a Lua validation function. Returns Ok(None) if valid,
    /// Ok(Some(message)) if invalid, Err on Lua error.
    fn run_validate_function(
        &self,
        func_ref: &str,
        value: &serde_json::Value,
        data: &HashMap<String, serde_json::Value>,
        collection: &str,
        field_name: &str,
    ) -> Result<Option<String>> {
        let lua = self.lua.lock()
            .map_err(|e| anyhow::anyhow!("Lua VM lock poisoned: {}", e))?;

        let parts: Vec<&str> = func_ref.split('.').collect();
        if parts.len() < 2 {
            return Err(anyhow::anyhow!(
                "Validate reference '{}' must be module.function format", func_ref
            ));
        }

        let module_path = parts[..parts.len() - 1].join(".");
        let func_name = parts[parts.len() - 1];

        let require: mlua::Function = lua.globals().get("require")?;
        let module: mlua::Table = require.call(module_path.clone())
            .with_context(|| format!("Failed to require module '{}'", module_path))?;
        let func: mlua::Function = module.get(func_name)
            .with_context(|| format!("Function '{}' not found in module '{}'", func_name, module_path))?;

        // Build the Lua value
        let lua_value = super::api::json_to_lua(&lua, value)?;

        // Build context table
        let ctx_table = lua.create_table()?;
        ctx_table.set("collection", collection)?;
        ctx_table.set("field_name", field_name)?;
        let data_table = lua.create_table()?;
        for (k, v) in data {
            data_table.set(k.as_str(), super::api::json_to_lua(&lua, v)?)?;
        }
        ctx_table.set("data", data_table)?;

        let result: Value = func.call((lua_value, ctx_table))?;

        match result {
            Value::Nil => Ok(None),
            Value::Boolean(true) => Ok(None),
            Value::Boolean(false) => Ok(Some("validation failed".to_string())),
            Value::String(s) => Ok(Some(s.to_str()?.to_string())),
            _ => Ok(None),
        }
    }
}

/// Convert hook context data (JSON values) back to string map for query functions.
/// Only includes fields that have parent table columns (skips array/has-many).
pub fn hook_ctx_to_string_map(ctx: &HookContext) -> HashMap<String, String> {
    ctx.data.iter().map(|(k, v)| {
        (k.clone(), match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
    }).collect()
}


/// Get the list of hook references for a given event.
fn get_hook_refs<'a>(hooks: &'a CollectionHooks, event: &HookEvent) -> &'a Vec<String> {
    match event {
        HookEvent::BeforeValidate => &hooks.before_validate,
        HookEvent::BeforeChange => &hooks.before_change,
        HookEvent::AfterChange => &hooks.after_change,
        HookEvent::BeforeRead => &hooks.before_read,
        HookEvent::AfterRead => &hooks.after_read,
        HookEvent::BeforeDelete => &hooks.before_delete,
        HookEvent::AfterDelete => &hooks.after_delete,
    }
}

/// Call all globally registered hooks for a given event.
/// Iterates `_crap_event_hooks[event]` and calls each function with the context.
/// Reuses the same context-to-table / table-to-context conversion as `call_hook_ref`.
fn call_registered_hooks(
    lua: &Lua,
    event: &HookEvent,
    mut context: HookContext,
) -> Result<HookContext> {
    let globals = lua.globals();
    let event_hooks: mlua::Table = match globals.get("_crap_event_hooks") {
        Ok(t) => t,
        Err(_) => return Ok(context),
    };

    let list: mlua::Table = match event_hooks.get::<Value>(event.as_str()) {
        Ok(Value::Table(t)) => t,
        _ => return Ok(context),
    };

    let len = list.raw_len();
    if len == 0 {
        return Ok(context);
    }

    for i in 1..=len {
        let func: mlua::Function = list.raw_get(i)
            .with_context(|| format!("registered hook at index {} is not a function", i))?;

        tracing::debug!(
            "Running registered {} hook #{} for {}",
            event.as_str(), i, context.collection
        );

        // Build context table (same format as call_hook_ref)
        let ctx_table = lua.create_table()?;
        ctx_table.set("collection", context.collection.as_str())?;
        ctx_table.set("operation", context.operation.as_str())?;
        let data_table = lua.create_table()?;
        for (k, v) in &context.data {
            data_table.set(k.as_str(), super::api::json_to_lua(lua, v)?)?;
        }
        ctx_table.set("data", data_table)?;

        let result: Value = func.call(ctx_table)?;

        // Parse result back (same as call_hook_ref)
        match result {
            Value::Table(tbl) => {
                let data_result: mlua::Result<mlua::Table> = tbl.get("data");
                if let Ok(data_tbl) = data_result {
                    let mut new_data = HashMap::new();
                    for pair in data_tbl.pairs::<String, Value>() {
                        let (k, v) = pair?;
                        new_data.insert(k, super::api::lua_to_json(lua, &v)?);
                    }
                    context = HookContext {
                        data: new_data,
                        ..context
                    };
                }
            }
            _ => {}
        }
    }

    Ok(context)
}

/// Shared implementation for `run_field_hooks` and `run_field_hooks_with_conn`.
/// Caller is responsible for locking the Lua VM and (optionally) setting TxContext.
fn run_field_hooks_inner(
    lua: &Lua,
    fields: &[FieldDefinition],
    event: &FieldHookEvent,
    data: &mut HashMap<String, serde_json::Value>,
    collection: &str,
    operation: &str,
) -> Result<()> {
    for field in fields {
        let hook_refs = get_field_hook_refs(&field.hooks, event);
        if hook_refs.is_empty() {
            continue;
        }

        let value = data.get(&field.name)
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let mut current = value;
        for hook_ref in hook_refs {
            tracing::debug!(
                "Running field hook: {} for {}.{}",
                hook_ref, collection, field.name
            );
            current = call_field_hook_ref(
                lua, hook_ref, current,
                &field.name, collection, operation, data,
            )?;
        }

        data.insert(field.name.clone(), current);
    }

    Ok(())
}

/// Get the list of field hook references for a given event.
fn get_field_hook_refs<'a>(hooks: &'a FieldHooks, event: &FieldHookEvent) -> &'a Vec<String> {
    match event {
        FieldHookEvent::BeforeValidate => &hooks.before_validate,
        FieldHookEvent::BeforeChange => &hooks.before_change,
        FieldHookEvent::AfterChange => &hooks.after_change,
        FieldHookEvent::AfterRead => &hooks.after_read,
    }
}

/// Resolve a dotted function reference and call it as a field hook.
/// Field hooks receive `(value, context)` and return the new value.
fn call_field_hook_ref(
    lua: &Lua,
    hook_ref: &str,
    value: serde_json::Value,
    field_name: &str,
    collection: &str,
    operation: &str,
    data: &HashMap<String, serde_json::Value>,
) -> Result<serde_json::Value> {
    let parts: Vec<&str> = hook_ref.split('.').collect();
    if parts.len() < 2 {
        return Err(anyhow::anyhow!(
            "Field hook reference '{}' must be module.function format", hook_ref
        ));
    }

    let module_path = parts[..parts.len() - 1].join(".");
    let func_name = parts[parts.len() - 1];

    let require: mlua::Function = lua.globals().get("require")?;
    let module: mlua::Table = require.call(module_path.clone())
        .with_context(|| format!("Failed to require module '{}'", module_path))?;
    let func: mlua::Function = module.get(func_name)
        .with_context(|| format!("Function '{}' not found in module '{}'", func_name, module_path))?;

    // Convert the field value to Lua
    let lua_value = super::api::json_to_lua(lua, &value)?;

    // Build context table
    let ctx_table = lua.create_table()?;
    ctx_table.set("field_name", field_name)?;
    ctx_table.set("collection", collection)?;
    ctx_table.set("operation", operation)?;
    let data_table = lua.create_table()?;
    for (k, v) in data {
        data_table.set(k.as_str(), super::api::json_to_lua(lua, v)?)?;
    }
    ctx_table.set("data", data_table)?;

    // Call: new_value = hook(value, context)
    let result: Value = func.call((lua_value, ctx_table))?;

    // Convert result back to JSON
    super::api::lua_to_json(lua, &result)
        .map_err(|e| anyhow::anyhow!("Field hook '{}' returned invalid value: {}", hook_ref, e))
}

/// Resolve a dotted function reference (e.g., "hooks.posts.auto_slug")
/// and call it with the context.
fn call_hook_ref(lua: &Lua, hook_ref: &str, context: HookContext) -> Result<HookContext> {
    let parts: Vec<&str> = hook_ref.split('.').collect();
    if parts.is_empty() {
        return Ok(context);
    }

    // "hooks.posts.auto_slug" -> require("hooks.posts").auto_slug
    let (module_path, func_name) = if parts.len() >= 2 {
        let module = parts[..parts.len() - 1].join(".");
        let func = parts[parts.len() - 1];
        (module, func)
    } else {
        return Err(anyhow::anyhow!(
            "Hook reference '{}' must be module.function format", hook_ref
        ));
    };

    let require: mlua::Function = lua.globals().get("require")?;
    let module: mlua::Table = require.call(module_path.clone())
        .with_context(|| format!("Failed to require module '{}'", module_path))?;

    let func: mlua::Function = module.get(func_name)
        .with_context(|| format!("Function '{}' not found in module '{}'", func_name, module_path))?;

    // Convert context to Lua table
    let ctx_table = lua.create_table()?;
    ctx_table.set("collection", context.collection.as_str())?;
    ctx_table.set("operation", context.operation.as_str())?;

    let data_table = lua.create_table()?;
    for (k, v) in &context.data {
        data_table.set(k.as_str(), super::api::json_to_lua(lua, v)?)?;
    }
    ctx_table.set("data", data_table)?;

    // Call the hook
    let result: Value = func.call(ctx_table)?;

    // Parse the result back
    match result {
        Value::Table(tbl) => {
            let data_result: mlua::Result<mlua::Table> = tbl.get("data");
            if let Ok(data_tbl) = data_result {
                let mut new_data = HashMap::new();
                for pair in data_tbl.pairs::<String, Value>() {
                    let (k, v) = pair?;
                    new_data.insert(k, super::api::lua_to_json(lua, &v)?);
                }
                Ok(HookContext {
                    data: new_data,
                    ..context
                })
            } else {
                Ok(context)
            }
        }
        _ => Ok(context),
    }
}

// ── Lua CRUD function registration ──────────────────────────────────────────

/// Get the active transaction connection from Lua app_data.
/// Returns an error if called outside of `run_hooks_with_conn`.
fn get_tx_conn(lua: &Lua) -> mlua::Result<*const rusqlite::Connection> {
    let ctx = lua.app_data_ref::<TxContext>()
        .ok_or_else(|| mlua::Error::RuntimeError(
            "crap.collections CRUD functions are only available inside hooks \
             with transaction context (before_change, before_delete, etc.)"
                .into()
        ))?;
    Ok(ctx.0)
}

/// Register the CRUD functions on `crap.collections` and `crap.globals`.
/// They read the active connection from Lua app_data (set by `run_hooks_with_conn`).
fn register_crud_functions(lua: &Lua, registry: SharedRegistry) -> Result<()> {
    let crap: mlua::Table = lua.globals().get("crap")?;
    let collections: mlua::Table = crap.get("collections")?;

    // crap.collections.find(collection, query?)
    // query.depth (optional, default 0): populate relationship fields to this depth
    {
        let reg = registry.clone();
        let find_fn = lua.create_function(move |lua, (collection, query_table): (String, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            // Safety: pointer is valid while TxContext is in app_data
            let conn = unsafe { &*conn_ptr };

            let depth: i32 = query_table.as_ref()
                .and_then(|qt| qt.get::<i32>("depth").ok())
                .unwrap_or(0)
                .min(10).max(0);

            let def = {
                let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                    format!("Registry lock: {}", e)
                ))?;
                r.get_collection(&collection)
                    .cloned()
                    .ok_or_else(|| mlua::Error::RuntimeError(
                        format!("Collection '{}' not found", collection)
                    ))?
            };

            let find_query = match query_table {
                Some(qt) => lua_table_to_find_query(&qt)?,
                None => FindQuery::default(),
            };

            query::validate_query_fields(&def, &find_query)
                .map_err(|e| mlua::Error::RuntimeError(format!("find error: {}", e)))?;

            let mut docs = query::find(conn, &collection, &def, &find_query)
                .map_err(|e| mlua::Error::RuntimeError(format!("find error: {}", e)))?;
            let total = query::count(conn, &collection, &def, &find_query.filters)
                .map_err(|e| mlua::Error::RuntimeError(format!("count error: {}", e)))?;

            // Hydrate join table data + populate relationships
            for doc in &mut docs {
                query::hydrate_document(conn, &collection, &def, doc)
                    .map_err(|e| mlua::Error::RuntimeError(format!("hydrate error: {}", e)))?;
            }
            if depth > 0 {
                let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                    format!("Registry lock: {}", e)
                ))?;
                for doc in &mut docs {
                    let mut visited = std::collections::HashSet::new();
                    query::populate_relationships(
                        conn, &r, &collection, &def, doc, depth, &mut visited,
                    ).map_err(|e| mlua::Error::RuntimeError(format!("populate error: {}", e)))?;
                }
            }

            find_result_to_lua(lua, &docs, total)
        })?;
        collections.set("find", find_fn)?;
    }

    // crap.collections.find_by_id(collection, id, opts?)
    // opts.depth (optional, default 0): populate relationship fields to this depth
    {
        let reg = registry.clone();
        let find_by_id_fn = lua.create_function(move |lua, (collection, id, opts): (String, String, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let depth: i32 = opts.as_ref()
                .and_then(|o| o.get::<i32>("depth").ok())
                .unwrap_or(0)
                .min(10).max(0);

            let def = {
                let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                    format!("Registry lock: {}", e)
                ))?;
                r.get_collection(&collection)
                    .cloned()
                    .ok_or_else(|| mlua::Error::RuntimeError(
                        format!("Collection '{}' not found", collection)
                    ))?
            };

            let mut doc = query::find_by_id(conn, &collection, &def, &id)
                .map_err(|e| mlua::Error::RuntimeError(format!("find_by_id error: {}", e)))?;

            if let Some(ref mut d) = doc {
                query::hydrate_document(conn, &collection, &def, d)
                    .map_err(|e| mlua::Error::RuntimeError(format!("hydrate error: {}", e)))?;
                if depth > 0 {
                    let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                        format!("Registry lock: {}", e)
                    ))?;
                    let mut visited = std::collections::HashSet::new();
                    query::populate_relationships(
                        conn, &r, &collection, &def, d, depth, &mut visited,
                    ).map_err(|e| mlua::Error::RuntimeError(format!("populate error: {}", e)))?;
                }
            }

            match doc {
                Some(d) => Ok(Value::Table(document_to_lua_table(lua, &d)?)),
                None => Ok(Value::Nil),
            }
        })?;
        collections.set("find_by_id", find_by_id_fn)?;
    }

    // crap.collections.create(collection, data)
    {
        let reg = registry.clone();
        let create_fn = lua.create_function(move |lua, (collection, data_table): (String, mlua::Table)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let def = {
                let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                    format!("Registry lock: {}", e)
                ))?;
                r.get_collection(&collection)
                    .cloned()
                    .ok_or_else(|| mlua::Error::RuntimeError(
                        format!("Collection '{}' not found", collection)
                    ))?
            };

            let data = lua_table_to_hashmap(&data_table)?;
            let doc = query::create(conn, &collection, &def, &data)
                .map_err(|e| mlua::Error::RuntimeError(format!("create error: {}", e)))?;

            document_to_lua_table(lua, &doc)
        })?;
        collections.set("create", create_fn)?;
    }

    // crap.collections.update(collection, id, data)
    {
        let reg = registry.clone();
        let update_fn = lua.create_function(move |lua, (collection, id, data_table): (String, String, mlua::Table)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let def = {
                let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                    format!("Registry lock: {}", e)
                ))?;
                r.get_collection(&collection)
                    .cloned()
                    .ok_or_else(|| mlua::Error::RuntimeError(
                        format!("Collection '{}' not found", collection)
                    ))?
            };

            let data = lua_table_to_hashmap(&data_table)?;
            let doc = query::update(conn, &collection, &def, &id, &data)
                .map_err(|e| mlua::Error::RuntimeError(format!("update error: {}", e)))?;

            document_to_lua_table(lua, &doc)
        })?;
        collections.set("update", update_fn)?;
    }

    // crap.collections.delete(collection, id)
    {
        let delete_fn = lua.create_function(move |lua, (collection, id): (String, String)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            query::delete(conn, &collection, &id)
                .map_err(|e| mlua::Error::RuntimeError(format!("delete error: {}", e)))?;

            Ok(true)
        })?;
        collections.set("delete", delete_fn)?;
    }

    // ── Globals CRUD ─────────────────────────────────────────────────────────

    let globals: mlua::Table = crap.get("globals")?;

    // crap.globals.get(slug)
    {
        let reg = registry.clone();
        let get_fn = lua.create_function(move |lua, slug: String| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let def = {
                let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                    format!("Registry lock: {}", e)
                ))?;
                r.get_global(&slug)
                    .cloned()
                    .ok_or_else(|| mlua::Error::RuntimeError(
                        format!("Global '{}' not found", slug)
                    ))?
            };

            let doc = query::get_global(conn, &slug, &def)
                .map_err(|e| mlua::Error::RuntimeError(format!("get_global error: {}", e)))?;

            document_to_lua_table(lua, &doc)
        })?;
        globals.set("get", get_fn)?;
    }

    // crap.globals.update(slug, data)
    {
        let reg = registry.clone();
        let update_fn = lua.create_function(move |lua, (slug, data_table): (String, mlua::Table)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let def = {
                let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                    format!("Registry lock: {}", e)
                ))?;
                r.get_global(&slug)
                    .cloned()
                    .ok_or_else(|| mlua::Error::RuntimeError(
                        format!("Global '{}' not found", slug)
                    ))?
            };

            let data = lua_table_to_hashmap(&data_table)?;
            let doc = query::update_global(conn, &slug, &def, &data)
                .map_err(|e| mlua::Error::RuntimeError(format!("update_global error: {}", e)))?;

            document_to_lua_table(lua, &doc)
        })?;
        globals.set("update", update_fn)?;
    }

    Ok(())
}

// ── Lua <-> Rust type conversion helpers ────────────────────────────────────

/// Convert a Lua query table to a FindQuery.
/// Supports both simple filters (`{ status = "published" }`) and operator-based
/// filters (`{ title = { contains = "hello" } }`).
fn lua_table_to_find_query(tbl: &mlua::Table) -> mlua::Result<FindQuery> {
    let filters = if let Ok(filters_tbl) = tbl.get::<mlua::Table>("filters") {
        let mut clauses = Vec::new();
        for pair in filters_tbl.pairs::<String, Value>() {
            let (field, value) = pair?;
            match value {
                // Simple string value -> Equals
                Value::String(s) => {
                    clauses.push(FilterClause::Single(Filter {
                        field,
                        op: FilterOp::Equals(s.to_str()?.to_string()),
                    }));
                }
                // Number -> Equals with string representation
                Value::Integer(i) => {
                    clauses.push(FilterClause::Single(Filter {
                        field,
                        op: FilterOp::Equals(i.to_string()),
                    }));
                }
                Value::Number(n) => {
                    clauses.push(FilterClause::Single(Filter {
                        field,
                        op: FilterOp::Equals(n.to_string()),
                    }));
                }
                // Table -> operator-based filter
                Value::Table(op_tbl) => {
                    for op_pair in op_tbl.pairs::<String, Value>() {
                        let (op_name, op_val) = op_pair?;
                        let op = lua_parse_filter_op(&op_name, &op_val)?;
                        clauses.push(FilterClause::Single(Filter {
                            field: field.clone(),
                            op,
                        }));
                    }
                }
                _ => {} // skip nil, bool, etc.
            }
        }
        clauses
    } else {
        Vec::new()
    };

    let order_by: Option<String> = tbl.get("order_by").ok();
    let limit: Option<i64> = tbl.get("limit").ok();
    let offset: Option<i64> = tbl.get("offset").ok();

    Ok(FindQuery { filters, order_by, limit, offset })
}

/// Parse a Lua filter operator name + value into a FilterOp.
fn lua_parse_filter_op(op_name: &str, value: &Value) -> mlua::Result<FilterOp> {
    let to_string = |v: &Value| -> mlua::Result<String> {
        match v {
            Value::String(s) => Ok(s.to_str()?.to_string()),
            Value::Integer(i) => Ok(i.to_string()),
            Value::Number(n) => Ok(n.to_string()),
            Value::Boolean(b) => Ok(b.to_string()),
            _ => Err(mlua::Error::RuntimeError("filter value must be string, number, or boolean".into())),
        }
    };

    match op_name {
        "equals" => Ok(FilterOp::Equals(to_string(value)?)),
        "not_equals" => Ok(FilterOp::NotEquals(to_string(value)?)),
        "like" => Ok(FilterOp::Like(to_string(value)?)),
        "contains" => Ok(FilterOp::Contains(to_string(value)?)),
        "greater_than" => Ok(FilterOp::GreaterThan(to_string(value)?)),
        "less_than" => Ok(FilterOp::LessThan(to_string(value)?)),
        "greater_than_or_equal" => Ok(FilterOp::GreaterThanOrEqual(to_string(value)?)),
        "less_than_or_equal" => Ok(FilterOp::LessThanOrEqual(to_string(value)?)),
        "in" => {
            if let Value::Table(t) = value {
                let mut vals = Vec::new();
                for v in t.clone().sequence_values::<Value>() {
                    vals.push(to_string(&v?)?);
                }
                Ok(FilterOp::In(vals))
            } else {
                Err(mlua::Error::RuntimeError("'in' operator requires a table/array".into()))
            }
        }
        "not_in" => {
            if let Value::Table(t) = value {
                let mut vals = Vec::new();
                for v in t.clone().sequence_values::<Value>() {
                    vals.push(to_string(&v?)?);
                }
                Ok(FilterOp::NotIn(vals))
            } else {
                Err(mlua::Error::RuntimeError("'not_in' operator requires a table/array".into()))
            }
        }
        "exists" => Ok(FilterOp::Exists),
        "not_exists" => Ok(FilterOp::NotExists),
        _ => Err(mlua::Error::RuntimeError(format!("unknown filter operator '{}'", op_name))),
    }
}

/// Convert a Lua data table to a HashMap<String, String> for create/update.
fn lua_table_to_hashmap(tbl: &mlua::Table) -> mlua::Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for pair in tbl.pairs::<String, Value>() {
        let (k, v) = pair?;
        let s = match v {
            Value::String(s) => s.to_str()?.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Boolean(b) => b.to_string(),
            Value::Nil => continue,
            _ => continue,
        };
        map.insert(k, s);
    }
    Ok(map)
}

/// Convert a Document to a Lua table.
fn document_to_lua_table(lua: &Lua, doc: &crate::core::Document) -> mlua::Result<mlua::Table> {
    let tbl = lua.create_table()?;
    tbl.set("id", doc.id.as_str())?;
    for (k, v) in &doc.fields {
        tbl.set(k.as_str(), super::api::json_to_lua(lua, v)?)?;
    }
    if let Some(ref ts) = doc.created_at {
        tbl.set("created_at", ts.as_str())?;
    }
    if let Some(ref ts) = doc.updated_at {
        tbl.set("updated_at", ts.as_str())?;
    }
    Ok(tbl)
}

/// Convert a find result (documents + total) to a Lua table.
fn find_result_to_lua(lua: &Lua, docs: &[crate::core::Document], total: i64) -> mlua::Result<mlua::Table> {
    let tbl = lua.create_table()?;
    let docs_tbl = lua.create_table()?;
    for (i, doc) in docs.iter().enumerate() {
        docs_tbl.set(i + 1, document_to_lua_table(lua, doc)?)?;
    }
    tbl.set("documents", docs_tbl)?;
    tbl.set("total", total)?;
    Ok(tbl)
}
