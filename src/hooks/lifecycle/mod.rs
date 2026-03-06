//! Hook execution engine: runs field, collection, and registered hooks within transactions.

pub mod crud;
pub mod access;
mod context;
mod validation;
mod vm_pool;

// Re-exports from extracted modules
pub use context::{HookContext, hook_ctx_to_string_map};
pub use validation::evaluate_condition_table;
// Re-exports for sibling modules (crud.rs, access.rs)
use validation::validate_fields_inner;
use context::{context_to_lua_table, read_context_back};
use vm_pool::VmPool;

use anyhow::{Context, Result};
use mlua::{Lua, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use crate::config::CrapConfig;
use crate::core::collection::{CollectionHooks, LiveSetting};
use crate::core::event::{EventBus, EventTarget, EventOperation, EventUser};
use crate::core::Document;
use crate::core::SharedRegistry;
use crate::core::field::{FieldDefinition, FieldHooks};
use crate::core::validate::{FieldError, ValidationError};
use crate::db::query::AccessResult;

/// Result of evaluating a display condition function.
#[derive(Debug, Clone)]
pub enum DisplayConditionResult {
    /// Lua returned a boolean. Must be re-evaluated server-side on changes.
    Bool(bool),
    /// Lua returned a condition table. Can be evaluated client-side.
    /// `visible` is the initial evaluation result; `condition` is the JSON to embed.
    Table { condition: serde_json::Value, visible: bool },
}

use crud::register_crud_functions;
use access::{check_access_with_lua, check_field_read_access_with_lua, check_field_write_access_with_lua};

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
    BeforeBroadcast,
    BeforeRender,
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
            HookEvent::BeforeBroadcast => "before_broadcast",
            HookEvent::BeforeRender => "before_render",
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

/// Raw pointer wrapper for injecting a transaction/connection into Lua CRUD
/// functions via `lua.set_app_data()`. Only valid between `set_app_data` and
/// `remove_app_data` calls in `run_hooks_with_conn`.
pub(super) struct TxContext(pub(super) *const rusqlite::Connection);

// Safety: TxContext is only stored in Lua app_data while the originating
// Connection/Transaction is alive and the Lua mutex is held. The pointer
// is never sent across threads independently.
unsafe impl Send for TxContext {}
unsafe impl Sync for TxContext {}

/// Optional authenticated user context injected alongside TxContext.
/// CRUD closures read this when overrideAccess = false.
pub(super) struct UserContext(pub(super) Option<Document>);
unsafe impl Send for UserContext {}
unsafe impl Sync for UserContext {}

/// Tracks hook recursion depth for Lua CRUD → hook → CRUD chains.
/// Stored in Lua `app_data` alongside `TxContext`.
pub(super) struct HookDepth(pub(super) u32);

/// Max allowed hook depth, read from config and stored in Lua `app_data`.
pub(super) struct MaxHookDepth(pub(super) u32);

/// Whether the system is in default-deny mode for access control.
/// Stored in Lua `app_data` so access checks can read it without signature changes.
pub(super) struct DefaultDeny(pub(super) bool);

/// Thread-safe hook runner with a pool of Lua VMs for concurrent execution.
#[derive(Clone)]
pub struct HookRunner {
    pool: Arc<VmPool>,
    /// Cached set of event names that have globally-registered hooks (from init.lua).
    /// Since hooks are only registered during VM creation (init.lua), this set is immutable.
    /// Allows skipping VM acquisition when no registered hooks exist for an event.
    registered_events: Arc<HashSet<String>>,
}

/// Create and fully initialize a single Lua VM with package paths, API, CRUD functions,
/// collection/global/job loading, and init.lua execution.
fn create_lua_vm(
    config_dir: &Path,
    registry: SharedRegistry,
    config: &CrapConfig,
    vm_index: usize,
) -> Result<Lua> {
    let lua = Lua::new();
    lua.set_app_data(crate::hooks::api::VmLabel(format!("vm-{}", vm_index)));

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
    crate::hooks::api::register_api(&lua, registry.clone(), config_dir, config)?;

    // Register CRUD functions on crap.collections (find, find_by_id, create, update, delete).
    // These read the active transaction from Lua app_data when called inside hooks.
    register_crud_functions(&lua, registry, &config.locale, config.hooks.max_depth, &config.pagination)?;

    // Initialize hook depth tracking
    lua.set_app_data(HookDepth(0));
    lua.set_app_data(MaxHookDepth(config.hooks.max_depth));
    lua.set_app_data(DefaultDeny(config.access.default_deny));

    // Auto-load collections/*.lua, globals/*.lua, and jobs/*.lua
    let collections_dir = config_dir.join("collections");
    if collections_dir.exists() {
        let _ = crate::hooks::load_lua_dir(&lua, &collections_dir, "collection")?;
    }
    let globals_dir = config_dir.join("globals");
    if globals_dir.exists() {
        let _ = crate::hooks::load_lua_dir(&lua, &globals_dir, "global")?;
    }
    let jobs_dir = config_dir.join("jobs");
    if jobs_dir.exists() {
        let _ = crate::hooks::load_lua_dir(&lua, &jobs_dir, "job")?;
    }

    // Execute init.lua so crap.hooks.register() calls take effect in this VM
    let init_path = config_dir.join("init.lua");
    if init_path.exists() {
        tracing::debug!("[lua:vm-{vm_index}] Executing init.lua");
        let code = std::fs::read_to_string(&init_path)
            .with_context(|| format!("Failed to read {}", init_path.display()))?;
        lua.load(&code)
            .set_name(init_path.to_string_lossy())
            .exec()
            .with_context(|| "HookRunner: failed to execute init.lua")?;
    }

    Ok(lua)
}

impl HookRunner {
    /// Create a new HookRunner with a pool of Lua VMs.
    /// Each VM is fully initialized with CRUD functions, hooks, and init.lua.
    pub fn new(config_dir: &Path, registry: SharedRegistry, config: &CrapConfig) -> Result<Self> {
        let pool_size = config.hooks.vm_pool_size.max(1);
        tracing::info!("HookRunner: creating pool of {} Lua VMs", pool_size);

        let mut vms = Vec::with_capacity(pool_size);
        for i in 0..pool_size {
            vms.push(create_lua_vm(config_dir, registry.clone(), config, i + 1)?);
        }

        // Cache which events have globally-registered hooks (from init.lua).
        // All VMs execute the same init.lua, so checking any VM suffices.
        let registered_events = scan_registered_events(&vms[0]);
        if !registered_events.is_empty() {
            tracing::info!("HookRunner: registered events: {:?}", registered_events);
        }

        Ok(Self {
            pool: Arc::new(VmPool::new(vms)),
            registered_events: Arc::new(registered_events),
        })
    }

    /// Check if any globally-registered hooks exist for the given event.
    /// Uses the cached set — no VM acquisition needed.
    #[inline]
    pub fn has_registered_hooks_for(&self, event: &str) -> bool {
        self.registered_events.contains(event)
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

        // Skip VM acquisition entirely when no work to do
        if hook_refs.is_empty() && !self.has_registered_hooks_for(event.as_str()) {
            return Ok(context);
        }

        let lua = self.pool.acquire()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

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
    /// `user` is the authenticated user (if any) — propagated to CRUD closures
    /// for `overrideAccess = false` enforcement.
    pub fn run_hooks_with_conn(
        &self,
        hooks: &CollectionHooks,
        event: HookEvent,
        mut context: HookContext,
        conn: &rusqlite::Connection,
        user: Option<&Document>,
    ) -> Result<HookContext> {
        let hook_refs = get_hook_refs(hooks, &event);

        // Skip VM acquisition entirely when no work to do
        if hook_refs.is_empty() && !self.has_registered_hooks_for(event.as_str()) {
            return Ok(context);
        }

        let lua = self.pool.acquire()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        // Inject the connection pointer so CRUD functions can use it.
        // Safety: conn is valid for the duration of this method, and we hold
        // the Lua mutex so no concurrent access is possible.
        lua.set_app_data(TxContext(conn as *const _));
        lua.set_app_data(UserContext(user.cloned()));

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
        lua.remove_app_data::<UserContext>();

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

        let lua = self.pool.acquire()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        lua.set_app_data(TxContext(conn as *const _));
        lua.set_app_data(UserContext(None));

        let result = (|| -> Result<()> {
            for hook_ref in refs {
                tracing::debug!("Running system hook: {}", hook_ref);
                let ctx = HookContext {
                    collection: String::new(),
                    operation: "init".to_string(),
                    data: HashMap::new(),
                    locale: None,
                    draft: None,
                    context: HashMap::new(),
                };
                call_hook_ref(&lua, hook_ref, ctx)?;
            }
            Ok(())
        })();

        lua.remove_app_data::<TxContext>();
        lua.remove_app_data::<UserContext>();

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
        // Skip VM acquisition if no fields have hooks for this event
        if !has_field_hooks_for_event(fields, &event) {
            return Ok(());
        }

        let lua = self.pool.acquire()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        run_field_hooks_inner(&lua, fields, &event, data, collection, operation)
    }

    /// Run field-level hooks with an active database connection/transaction injected.
    /// CRUD functions (`crap.collections.find`, `.create`, etc.) become available
    /// to Lua field hooks, sharing the provided connection for transaction atomicity.
    #[allow(clippy::too_many_arguments)]
    pub fn run_field_hooks_with_conn(
        &self,
        fields: &[FieldDefinition],
        event: FieldHookEvent,
        data: &mut HashMap<String, serde_json::Value>,
        collection: &str,
        operation: &str,
        conn: &rusqlite::Connection,
        user: Option<&Document>,
    ) -> Result<()> {
        // Skip VM acquisition if no fields have hooks for this event
        if !has_field_hooks_for_event(fields, &event) {
            return Ok(());
        }

        let lua = self.pool.acquire()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        // Inject the connection pointer so CRUD functions can use it.
        lua.set_app_data(TxContext(conn as *const _));
        lua.set_app_data(UserContext(user.cloned()));

        let result = run_field_hooks_inner(&lua, fields, &event, data, collection, operation);

        // Always clean up, even on error.
        lua.remove_app_data::<TxContext>();
        lua.remove_app_data::<UserContext>();

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
            locale: None,
            draft: None,
            context: HashMap::new(),
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
        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("VM pool error in apply_after_read: {}", e);
                return doc;
            }
        };
        apply_after_read_inner(&lua, hooks, fields, collection, operation, doc)
    }

    /// Fire after_read hooks on a list of documents.
    /// Acquires a single VM for the entire batch instead of one per document.
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
        let has_collection_hooks = !hooks.after_read.is_empty();
        let has_registered = self.has_registered_hooks_for("after_read");

        // No hooks at all — skip VM acquisition entirely
        if !has_field_hooks && !has_collection_hooks && !has_registered {
            return docs;
        }

        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("VM pool error in apply_after_read_many: {}", e);
                return docs;
            }
        };

        docs.into_iter()
            .map(|doc| apply_after_read_inner(&lua, hooks, fields, collection, operation, doc))
            .collect()
    }

    /// Run the full before-write lifecycle:
    ///   field BeforeValidate → collection BeforeValidate → validate_fields →
    ///   field BeforeChange → collection BeforeChange.
    /// Returns the final hook context with validated, hook-processed data.
    /// Callers use `hook_ctx_to_string_map()` on the result to get the data for query functions.
    ///
    /// Field hooks in before-write get full CRUD access (same transaction).
    /// `user` is the authenticated user — propagated to CRUD closures for `overrideAccess`.
    #[allow(clippy::too_many_arguments)]
    pub fn run_before_write(
        &self,
        hooks: &CollectionHooks,
        fields: &[FieldDefinition],
        mut ctx: HookContext,
        conn: &rusqlite::Connection,
        table: &str,
        exclude_id: Option<&str>,
        user: Option<&Document>,
        is_draft: bool,
    ) -> Result<HookContext> {
        // Field-level before_validate (normalize inputs, CRUD available)
        self.run_field_hooks_with_conn(
            fields, FieldHookEvent::BeforeValidate,
            &mut ctx.data, &ctx.collection, &ctx.operation, conn, user,
        )?;
        // Collection-level before_validate
        let ctx = self.run_hooks_with_conn(hooks, HookEvent::BeforeValidate, ctx, conn, user)?;
        // Validation (skip required checks for drafts)
        self.validate_fields(fields, &ctx.data, conn, table, exclude_id, is_draft)?;
        // Field-level before_change (post-validation transforms, CRUD available)
        let mut ctx = ctx;
        self.run_field_hooks_with_conn(
            fields, FieldHookEvent::BeforeChange,
            &mut ctx.data, &ctx.collection, &ctx.operation, conn, user,
        )?;
        // Collection-level before_change
        self.run_hooks_with_conn(hooks, HookEvent::BeforeChange, ctx, conn, user)
    }

    /// Run after-write hooks inside the transaction (with CRUD access).
    /// Field-level after_change hooks run first, then collection-level, then registered.
    /// Errors propagate up and cause the caller's transaction to roll back.
    #[allow(clippy::too_many_arguments)]
    pub fn run_after_write(
        &self,
        hooks: &CollectionHooks,
        fields: &[FieldDefinition],
        event: HookEvent,
        ctx: HookContext,
        conn: &rusqlite::Connection,
        user: Option<&Document>,
    ) -> Result<HookContext> {
        // Run field-level after_change hooks (with CRUD access)
        if matches!(event, HookEvent::AfterChange) {
            let has_field_hooks = fields.iter()
                .any(|f| !f.hooks.after_change.is_empty());
            if has_field_hooks {
                let mut data = ctx.data.clone();
                self.run_field_hooks_with_conn(
                    fields, FieldHookEvent::AfterChange,
                    &mut data, &ctx.collection, &ctx.operation, conn, user,
                )?;
            }
        }

        // Run collection-level + registered hooks (with CRUD access)
        self.run_hooks_with_conn(hooks, event, ctx, conn, user)
    }

    /// Run before_broadcast hooks. Returns Ok(Some(data)) to broadcast (possibly
    /// with transformed data), or Ok(None) to suppress the event.
    /// No CRUD access (fires after commit, same as after_change).
    pub fn run_before_broadcast(
        &self,
        hooks: &CollectionHooks,
        collection: &str,
        operation: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Option<HashMap<String, serde_json::Value>>> {
        let hook_refs = get_hook_refs(hooks, &HookEvent::BeforeBroadcast);

        // Skip VM acquisition entirely when no work to do
        if hook_refs.is_empty() && !self.has_registered_hooks_for("before_broadcast") {
            return Ok(Some(data));
        }

        let ctx = HookContext {
            collection: collection.to_string(),
            operation: operation.to_string(),
            data,
            locale: None,
            draft: None,
            context: HashMap::new(),
        };

        let lua = self.pool.acquire()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let mut context = ctx;

        // Run collection-level hook refs first
        for hook_ref in hook_refs {
            tracing::debug!("Running before_broadcast hook: {} for {}", hook_ref, context.collection);
            match call_before_broadcast_hook(&lua, hook_ref, context.clone())? {
                Some(new_ctx) => context = new_ctx,
                None => return Ok(None), // suppressed
            }
        }

        // Run global registered hooks
        match call_registered_before_broadcast(&lua, context)? {
            Some(ctx) => Ok(Some(ctx.data)),
            None => Ok(None),
        }
    }

    /// Check if a live event should be broadcast for this mutation.
    /// Returns Ok(true) to broadcast, Ok(false) to suppress.
    /// Runs WITHOUT transaction access (after write committed).
    pub fn check_live_setting(
        &self,
        live: Option<&LiveSetting>,
        collection: &str,
        operation: &str,
        data: &HashMap<String, serde_json::Value>,
    ) -> Result<bool> {
        match live {
            None => Ok(true), // absent = broadcast all
            Some(LiveSetting::Disabled) => Ok(false),
            Some(LiveSetting::Function(func_ref)) => {
                let lua = self.pool.acquire()
                    .map_err(|e| anyhow::anyhow!("{}", e))?;

                let func = resolve_hook_function(&lua, func_ref)?;

                let ctx_table = lua.create_table()?;
                ctx_table.set("collection", collection)?;
                ctx_table.set("operation", operation)?;
                let data_table = lua.create_table()?;
                for (k, v) in data {
                    data_table.set(k.as_str(), crate::hooks::api::json_to_lua(&lua, v)?)?;
                }
                ctx_table.set("data", data_table)?;

                let result: Value = func.call(ctx_table)?;
                match result {
                    Value::Boolean(b) => Ok(b),
                    Value::Nil => Ok(false),
                    _ => Ok(true),
                }
            }
        }
    }

    /// Publish a mutation event: check live setting → run before_broadcast hooks → EventBus.publish().
    /// Spawns into a background task (non-blocking, like fire_after_event).
    /// Untestable: spawns tokio::task::spawn_blocking for async event dispatch.
    #[allow(clippy::too_many_arguments)]
    #[cfg(not(tarpaulin_include))]
    pub fn publish_event(
        &self,
        event_bus: &Option<EventBus>,
        hooks: &CollectionHooks,
        live_setting: Option<&LiveSetting>,
        target: EventTarget,
        operation: EventOperation,
        collection: String,
        document_id: String,
        data: HashMap<String, serde_json::Value>,
        edited_by: Option<EventUser>,
    ) {
        let bus = match event_bus {
            Some(b) => b.clone(),
            None => return,
        };

        let runner = self.clone();
        let hooks = hooks.clone();
        let live = live_setting.cloned();
        let op_str = match &operation {
            EventOperation::Create => "create",
            EventOperation::Update => "update",
            EventOperation::Delete => "delete",
        }.to_string();

        tokio::task::spawn_blocking(move || {
            // 1. Check live setting
            match runner.check_live_setting(live.as_ref(), &collection, &op_str, &data) {
                Ok(false) => return,
                Err(e) => {
                    tracing::warn!("live setting check error for {}: {}", collection, e);
                    return;
                }
                Ok(true) => {}
            }

            // 2. Run before_broadcast hooks
            let broadcast_data = match runner.run_before_broadcast(&hooks, &collection, &op_str, data) {
                Ok(Some(d)) => d,
                Ok(None) => return, // suppressed
                Err(e) => {
                    tracing::warn!("before_broadcast hook error for {}: {}", collection, e);
                    return;
                }
            };

            // 3. Publish to EventBus
            bus.publish(target, operation, collection, document_id, broadcast_data, edited_by);
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
        let lua = self.pool.acquire()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        // Inject connection for CRUD access
        lua.set_app_data(TxContext(conn as *const _));
        lua.set_app_data(UserContext(None));

        let result = (|| -> Result<Option<Document>> {
            let func = resolve_hook_function(&lua, authenticate_ref)?;

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
                        fields.insert(k, crate::hooks::api::lua_to_json(&lua, &v)?);
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
        lua.remove_app_data::<UserContext>();
        result
    }

    /// Call a Lua function to compute a row label for an array/blocks row.
    /// Returns None if the function errors or returns nil.
    /// No CRUD access — pure formatting function.
    pub fn call_row_label(&self, func_ref: &str, row_data: &serde_json::Value) -> Option<String> {
        let lua = self.pool.acquire().ok()?;
        let func = resolve_hook_function(&lua, func_ref).ok()?;
        let row_lua = crate::hooks::api::json_to_lua(&lua, row_data).ok()?;
        match func.call::<Value>(row_lua) {
            Ok(Value::String(s)) => s.to_str().ok().map(|s| s.to_string()),
            _ => None,
        }
    }

    /// Evaluate a display condition function.
    /// Returns `DisplayConditionResult::Bool(visible)` or
    /// `DisplayConditionResult::Table { condition, visible }` depending on what Lua returns.
    /// No CRUD access — pure evaluation function.
    pub fn call_display_condition(
        &self,
        func_ref: &str,
        form_data: &serde_json::Value,
    ) -> Option<DisplayConditionResult> {
        let lua = self.pool.acquire().ok()?;
        call_display_condition_with_lua(&lua, func_ref, form_data)
    }

    /// Evaluate display conditions for multiple fields using a single VM acquisition.
    /// Returns a map from func_ref to the evaluation result.
    pub fn call_display_conditions_batch(
        &self,
        conditions: &[(&str, &serde_json::Value)],
    ) -> HashMap<String, DisplayConditionResult> {
        if conditions.is_empty() {
            return HashMap::new();
        }
        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(_) => return HashMap::new(),
        };
        let mut results = HashMap::new();
        for &(func_ref, form_data) in conditions {
            if let Some(result) = call_display_condition_with_lua(&lua, func_ref, form_data) {
                results.insert(func_ref.to_string(), result);
            }
        }
        results
    }

    /// Run `before_render` hooks on the template context.
    /// Global registered `before_render` hooks receive the full template context as a
    /// Lua table and return the (potentially modified) context. No CRUD access.
    /// On error: logs warning, returns original context unmodified.
    pub fn run_before_render(&self, mut context: serde_json::Value) -> serde_json::Value {
        // Skip VM acquisition entirely when no before_render hooks are registered
        if !self.has_registered_hooks_for("before_render") {
            return context;
        }

        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("VM pool error in run_before_render: {}", e);
                return context;
            }
        };

        if !has_registered_hooks(&lua, "before_render") {
            return context;
        }

        // Get the registered hooks table
        let hooks_table: mlua::Table = match lua.globals().get::<mlua::Table>("_crap_event_hooks")
            .and_then(|t| t.get::<mlua::Table>("before_render"))
        {
            Ok(t) => t,
            Err(_) => return context,
        };

        let len = hooks_table.raw_len();
        for i in 1..=len {
            let func: mlua::Function = match hooks_table.raw_get(i) {
                Ok(f) => f,
                Err(_) => continue,
            };

            let ctx_lua = match crate::hooks::api::json_to_lua(&lua, &context) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("before_render: failed to convert context to Lua: {}", e);
                    return context;
                }
            };

            match func.call::<Value>(ctx_lua) {
                Ok(Value::Table(tbl)) => {
                    match crate::hooks::api::lua_to_json(&lua, &Value::Table(tbl)) {
                        Ok(new_ctx) => context = new_ctx,
                        Err(e) => {
                            tracing::warn!("before_render: failed to convert Lua result to JSON: {}", e);
                        }
                    }
                }
                Ok(Value::Nil) => {
                    // Hook returned nil — keep context unchanged
                }
                Ok(_) => {
                    tracing::warn!("before_render hook returned non-table, non-nil value; ignoring");
                }
                Err(e) => {
                    tracing::warn!("before_render hook error: {}", e);
                }
            }
        }

        context
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
        if access_ref.is_none() {
            return Ok(AccessResult::Allowed);
        }

        let lua = self.pool.acquire()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        // Inject connection for CRUD access in access functions
        lua.set_app_data(TxContext(conn as *const _));
        lua.set_app_data(UserContext(None));

        let result = check_access_with_lua(&lua, access_ref, user, id, data);

        lua.remove_app_data::<TxContext>();
        lua.remove_app_data::<UserContext>();
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
        // Skip VM acquisition if no fields have read access functions
        if fields.iter().all(|f| f.access.read.is_none()) {
            return Vec::new();
        }
        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(_) => return Vec::new(),
        };
        lua.set_app_data(TxContext(conn as *const _));
        lua.set_app_data(UserContext(None));

        let result = check_field_read_access_with_lua(&lua, fields, user);

        lua.remove_app_data::<TxContext>();
        lua.remove_app_data::<UserContext>();
        result
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
        // Skip VM acquisition if no fields have write access functions for this operation
        let has_write_access = fields.iter().any(|f| match operation {
            "create" => f.access.create.is_some(),
            "update" => f.access.update.is_some(),
            _ => false,
        });
        if !has_write_access {
            return Vec::new();
        }
        let lua = match self.pool.acquire() {
            Ok(l) => l,
            Err(_) => return Vec::new(),
        };
        lua.set_app_data(TxContext(conn as *const _));
        lua.set_app_data(UserContext(None));

        let result = check_field_write_access_with_lua(&lua, fields, user, operation);

        lua.remove_app_data::<TxContext>();
        lua.remove_app_data::<UserContext>();
        result
    }

    /// Run a migration file (up or down direction) within a transaction.
    /// Loads the Lua file, calls `M.up()` or `M.down()` with CRUD access.
    pub fn run_migration(
        &self,
        path: &Path,
        direction: &str,
        conn: &rusqlite::Connection,
    ) -> Result<()> {
        let code = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read migration {}", path.display()))?;

        let lua = self.pool.acquire()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        // Inject connection for CRUD access
        lua.set_app_data(TxContext(conn as *const _));
        lua.set_app_data(UserContext(None));

        let result = (|| -> Result<()> {
            // Load the migration module
            let chunk = lua.load(&code).set_name(path.to_string_lossy());
            let module: mlua::Table = chunk.eval()
                .with_context(|| format!("Failed to load migration {}", path.display()))?;

            // Call M.up() or M.down()
            let func: mlua::Function = module.get(direction)
                .with_context(|| format!(
                    "Migration {} does not have a '{}' function",
                    path.display(), direction
                ))?;

            func.call::<()>(())
                .with_context(|| format!(
                    "Migration {}.{}() failed",
                    path.display(), direction
                ))?;

            Ok(())
        })();

        lua.remove_app_data::<TxContext>();
        lua.remove_app_data::<UserContext>();

        result
    }

    /// Execute a job handler function with CRUD access via TxContext.
    /// The handler receives a context table `{ data, job = { slug, attempt, max_attempts, queued_at } }`.
    /// Returns the handler's return value as JSON string (or None if nil).
    pub fn run_job_handler(
        &self,
        handler_ref: &str,
        slug: &str,
        data_json: &str,
        attempt: u32,
        max_attempts: u32,
        conn: &rusqlite::Connection,
    ) -> Result<Option<String>> {
        let lua = self.pool.acquire()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        lua.set_app_data(TxContext(conn as *const _));
        lua.set_app_data(UserContext(None));

        let result = (|| -> Result<Option<String>> {
            // Build context table
            let ctx = lua.create_table()?;

            // Parse data JSON into Lua table
            let data_value: serde_json::Value = serde_json::from_str(data_json)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            let data_lua = crate::hooks::api::json_to_lua(&lua, &data_value)?;
            ctx.set("data", data_lua)?;

            // Job metadata
            let job_meta = lua.create_table()?;
            job_meta.set("slug", slug)?;
            job_meta.set("attempt", attempt)?;
            job_meta.set("max_attempts", max_attempts)?;
            ctx.set("job", job_meta)?;

            // Resolve the handler function (e.g., "jobs.cleanup.run")
            let func = resolve_hook_function(&lua, handler_ref)?;

            // Call handler(ctx)
            let return_val: mlua::Value = func.call(ctx)?;

            // Convert return value to JSON
            match return_val {
                mlua::Value::Nil => Ok(None),
                other => {
                    let json_val = crate::hooks::api::lua_to_json(&lua, &other)?;
                    Ok(Some(serde_json::to_string(&json_val)?))
                }
            }
        })();

        lua.remove_app_data::<TxContext>();
        lua.remove_app_data::<UserContext>();

        result
    }

    /// Execute arbitrary Lua code within a transaction + user context.
    /// The Lua code must return a string. Useful for testing CRUD closures.
    #[allow(dead_code)]
    pub fn eval_lua_with_conn(
        &self,
        code: &str,
        conn: &rusqlite::Connection,
        user: Option<&Document>,
    ) -> Result<String> {
        let lua = self.pool.acquire()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        lua.set_app_data(TxContext(conn as *const _));
        lua.set_app_data(UserContext(user.cloned()));

        let result = lua.load(code).eval::<String>();

        lua.remove_app_data::<TxContext>();
        lua.remove_app_data::<UserContext>();

        result.map_err(|e| anyhow::anyhow!("{}", e))
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
        is_draft: bool,
    ) -> Result<(), ValidationError> {
        let lua = self.pool.acquire()
            .map_err(|_| ValidationError { errors: vec![FieldError {
                field: "_system".into(),
                message: "VM pool error".into(),
            }] })?;
        validate_fields_inner(&lua, fields, data, conn, table, exclude_id, is_draft)
    }
}

/// Inner implementation of `apply_after_read` — operates on a locked `&Lua`.
/// Runs field-level after_read hooks, then collection-level, then global registered.
/// On error: logs warning, returns original doc unmodified.
pub(super) fn apply_after_read_inner(
    lua: &Lua,
    hooks: &CollectionHooks,
    fields: &[FieldDefinition],
    collection: &str,
    operation: &str,
    doc: Document,
) -> Document {
    let has_field_hooks = fields.iter()
        .any(|f| !f.hooks.after_read.is_empty());

    let has_collection_hooks = !hooks.after_read.is_empty();
    let has_registered = has_registered_hooks(lua, "after_read");

    if !has_field_hooks && !has_collection_hooks && !has_registered {
        return doc;
    }

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
        if let Err(e) = run_field_hooks_inner(
            lua, fields, &FieldHookEvent::AfterRead,
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
        locale: None,
        draft: None,
        context: HashMap::new(),
    };

    // Run collection-level + global registered hooks
    let hook_refs = get_hook_refs(hooks, &HookEvent::AfterRead);
    let result = (|| -> Result<HookContext> {
        let mut context = ctx;
        for hook_ref in hook_refs {
            context = call_hook_ref(lua, hook_ref, context)?;
        }
        context = call_registered_hooks(lua, &HookEvent::AfterRead, context)?;
        Ok(context)
    })();

    match result {
        Ok(result_ctx) => {
            let mut fields = result_ctx.data;
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

/// Inner implementation of `run_hooks` / `run_hooks_with_conn` — operates on a locked `&Lua`.
/// Runs collection-level hook refs, then global registered hooks.
/// TxContext must already be set in app_data if CRUD access is needed.
pub(super) fn run_hooks_inner(
    lua: &Lua,
    hooks: &CollectionHooks,
    event: HookEvent,
    mut context: HookContext,
) -> Result<HookContext> {
    let hook_refs = get_hook_refs(hooks, &event);

    for hook_ref in hook_refs {
        tracing::debug!("Running hook (inner): {} for {}", hook_ref, context.collection);
        context = call_hook_ref(lua, hook_ref, context)?;
    }

    // Run global registered hooks
    context = call_registered_hooks(lua, &event, context)?;

    Ok(context)
}

/// Get the list of hook references for a given event.
fn get_hook_refs<'a>(hooks: &'a CollectionHooks, event: &HookEvent) -> &'a [String] {
    match event {
        HookEvent::BeforeValidate => &hooks.before_validate,
        HookEvent::BeforeChange => &hooks.before_change,
        HookEvent::AfterChange => &hooks.after_change,
        HookEvent::BeforeRead => &hooks.before_read,
        HookEvent::AfterRead => &hooks.after_read,
        HookEvent::BeforeDelete => &hooks.before_delete,
        HookEvent::AfterDelete => &hooks.after_delete,
        HookEvent::BeforeBroadcast => &hooks.before_broadcast,
        HookEvent::BeforeRender => &[], // global-only, no collection-level refs
    }
}

/// Check if any globally registered hooks exist for the given event.
fn has_registered_hooks(lua: &Lua, event: &str) -> bool {
    let globals = lua.globals();
    let event_hooks: mlua::Table = match globals.get("_crap_event_hooks") {
        Ok(t) => t,
        Err(_) => return false,
    };
    match event_hooks.get::<Value>(event) {
        Ok(Value::Table(t)) => t.raw_len() > 0,
        _ => false,
    }
}

/// Inner implementation of display condition evaluation — operates on a locked `&Lua`.
fn call_display_condition_with_lua(
    lua: &Lua,
    func_ref: &str,
    form_data: &serde_json::Value,
) -> Option<DisplayConditionResult> {
    let func = resolve_hook_function(lua, func_ref).ok()?;
    let data_lua = crate::hooks::api::json_to_lua(lua, form_data).ok()?;
    match func.call::<Value>(data_lua) {
        Ok(Value::Boolean(b)) => Some(DisplayConditionResult::Bool(b)),
        Ok(val @ Value::Table(_)) => {
            let json = crate::hooks::api::lua_to_json(lua, &val).ok()?;
            let visible = evaluate_condition_table(&json, form_data);
            Some(DisplayConditionResult::Table { condition: json, visible })
        }
        _ => None, // error or nil → show field (safe default)
    }
}

/// Check if any fields have hooks registered for the given field-level event.
fn has_field_hooks_for_event(fields: &[FieldDefinition], event: &FieldHookEvent) -> bool {
    fields.iter().any(|f| {
        let hooks = &f.hooks;
        match event {
            FieldHookEvent::BeforeValidate => !hooks.before_validate.is_empty(),
            FieldHookEvent::BeforeChange => !hooks.before_change.is_empty(),
            FieldHookEvent::AfterChange => !hooks.after_change.is_empty(),
            FieldHookEvent::AfterRead => !hooks.after_read.is_empty(),
        }
    })
}

/// Scan a Lua VM's `_crap_event_hooks` table and return the set of event names
/// that have at least one registered handler. Called once during HookRunner::new().
fn scan_registered_events(lua: &Lua) -> HashSet<String> {
    let mut events = HashSet::new();
    let globals = lua.globals();
    let event_hooks: mlua::Table = match globals.get("_crap_event_hooks") {
        Ok(t) => t,
        Err(_) => return events,
    };
    for pair in event_hooks.pairs::<String, Value>() {
        if let Ok((key, Value::Table(t))) = pair {
            if t.raw_len() > 0 {
                events.insert(key);
            }
        }
    }
    events
}

/// Call a before_broadcast hook ref. Returns Some(context) to continue, None to suppress.
fn call_before_broadcast_hook(
    lua: &Lua,
    hook_ref: &str,
    context: HookContext,
) -> Result<Option<HookContext>> {
    let func = resolve_hook_function(lua, hook_ref)?;

    let ctx_table = context_to_lua_table(lua, &context)?;
    let result: Value = func.call(ctx_table)?;

    match result {
        Value::Boolean(false) | Value::Nil => Ok(None),
        Value::Table(tbl) => {
            let data_result: mlua::Result<mlua::Table> = tbl.get("data");
            if let Ok(data_tbl) = data_result {
                let mut new_data = HashMap::new();
                for pair in data_tbl.pairs::<String, Value>() {
                    let (k, v) = pair?;
                    new_data.insert(k, crate::hooks::api::lua_to_json(lua, &v)?);
                }
                Ok(Some(HookContext { data: new_data, ..context }))
            } else {
                Ok(Some(context))
            }
        }
        _ => Ok(Some(context)),
    }
}

/// Call all globally registered before_broadcast hooks.
/// Returns Some(context) to continue, None if any hook suppresses.
fn call_registered_before_broadcast(
    lua: &Lua,
    mut context: HookContext,
) -> Result<Option<HookContext>> {
    let globals = lua.globals();
    let event_hooks: mlua::Table = match globals.get("_crap_event_hooks") {
        Ok(t) => t,
        Err(_) => return Ok(Some(context)),
    };

    let list: mlua::Table = match event_hooks.get::<Value>("before_broadcast") {
        Ok(Value::Table(t)) => t,
        _ => return Ok(Some(context)),
    };

    let len = list.raw_len();
    if len == 0 {
        return Ok(Some(context));
    }

    for i in 1..=len {
        let func: mlua::Function = list.raw_get(i)
            .with_context(|| format!("registered before_broadcast hook at index {} is not a function", i))?;

        let ctx_table = context_to_lua_table(lua, &context)?;

        let result: Value = func.call(ctx_table)?;

        match result {
            Value::Boolean(false) | Value::Nil => return Ok(None),
            Value::Table(tbl) => {
                let data_result: mlua::Result<mlua::Table> = tbl.get("data");
                if let Ok(data_tbl) = data_result {
                    let mut new_data = HashMap::new();
                    for pair in data_tbl.pairs::<String, Value>() {
                        let (k, v) = pair?;
                        new_data.insert(k, crate::hooks::api::lua_to_json(lua, &v)?);
                    }
                    context = HookContext { data: new_data, ..context };
                }
            }
            _ => {}
        }
    }

    Ok(Some(context))
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

        let ctx_table = context_to_lua_table(lua, &context)?;

        let result: Value = func.call(ctx_table)?;
        if let Value::Table(tbl) = result {
            let data_result: mlua::Result<mlua::Table> = tbl.get("data");
            if let Ok(data_tbl) = data_result {
                let mut new_data = HashMap::new();
                for pair in data_tbl.pairs::<String, Value>() {
                    let (k, v) = pair?;
                    new_data.insert(k, crate::hooks::api::lua_to_json(lua, &v)?);
                }
                context.data = new_data;
            }
            read_context_back(lua, &tbl, &mut context.context);
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

        let was_present = data.contains_key(&field.name);
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

        // Only write back if the field was already in the data, or the hook
        // produced a non-null value (e.g. auto_slug generating a slug on create).
        // Without this, absent fields on partial updates get coerced to Null,
        // which breaks the "skip required check for absent fields" logic.
        if was_present || !current.is_null() {
            data.insert(field.name.clone(), current);
        }
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

/// Resolve a hook reference and call it as a field hook.
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
    let func = resolve_hook_function(lua, hook_ref)?;

    // Convert the field value to Lua
    let lua_value = crate::hooks::api::json_to_lua(lua, &value)?;

    // Build context table
    let ctx_table = lua.create_table()?;
    ctx_table.set("field_name", field_name)?;
    ctx_table.set("collection", collection)?;
    ctx_table.set("operation", operation)?;
    let data_table = lua.create_table()?;
    for (k, v) in data {
        data_table.set(k.as_str(), crate::hooks::api::json_to_lua(lua, v)?)?;
    }
    ctx_table.set("data", data_table)?;

    // Call: new_value = hook(value, context)
    let result: Value = func.call((lua_value, ctx_table))?;

    // Convert result back to JSON
    crate::hooks::api::lua_to_json(lua, &result)
        .map_err(|e| anyhow::anyhow!("Field hook '{}' returned invalid value: {}", hook_ref, e))
}

/// Resolve a hook reference to a Lua function.
///
/// Tries file-per-hook first: `require("hooks.posts.auto_slug")` → function.
/// Falls back to module pattern: `require("hooks.posts")["auto_slug"]`.
pub(super) fn resolve_hook_function(lua: &Lua, hook_ref: &str) -> Result<mlua::Function> {
    let require: mlua::Function = lua.globals().get("require")?;

    // Try file-per-hook: require("hooks.posts.auto_slug") → function
    if let Ok(value) = require.call::<Value>(hook_ref) {
        if let Value::Function(f) = value {
            return Ok(f);
        }
    }

    // Fallback: module.function pattern
    let parts: Vec<&str> = hook_ref.split('.').collect();
    if parts.len() < 2 {
        anyhow::bail!("Hook ref '{}' must be module.function format", hook_ref);
    }
    let module_path = parts[..parts.len() - 1].join(".");
    let func_name = parts[parts.len() - 1];

    let module: mlua::Table = require.call(module_path.clone())
        .with_context(|| format!("Failed to require module '{}'", module_path))?;
    let func: mlua::Function = module.get(func_name)
        .with_context(|| format!("Function '{}' not found in module '{}'", func_name, module_path))?;
    Ok(func)
}

/// Resolve a dotted function reference (e.g., "hooks.posts.auto_slug")
/// and call it with the context.
fn call_hook_ref(lua: &Lua, hook_ref: &str, context: HookContext) -> Result<HookContext> {
    let func = resolve_hook_function(lua, hook_ref)?;

    // Convert context to Lua table
    let ctx_table = context_to_lua_table(lua, &context)?;

    // Call the hook
    let result: Value = func.call(ctx_table)?;

    // Parse the result back
    match result {
        Value::Table(tbl) => {
            let mut new_ctx = context;
            let data_result: mlua::Result<mlua::Table> = tbl.get("data");
            if let Ok(data_tbl) = data_result {
                let mut new_data = HashMap::new();
                for pair in data_tbl.pairs::<String, Value>() {
                    let (k, v) = pair?;
                    new_data.insert(k, crate::hooks::api::lua_to_json(lua, &v)?);
                }
                new_ctx.data = new_data;
            }
            read_context_back(lua, &tbl, &mut new_ctx.context);
            Ok(new_ctx)
        }
        _ => Ok(context),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- apply_after_read_inner tests ---

    #[test]
    fn test_apply_after_read_no_hooks_returns_unchanged() {
        let lua = mlua::Lua::new();
        // Initialize the _crap_event_hooks table
        lua.load("_crap_event_hooks = {}").exec().unwrap();
        let hooks = CollectionHooks::default();
        let fields = vec![FieldDefinition {
            name: "title".to_string(),
            ..Default::default()
        }];
        let mut doc = crate::core::Document::new("doc1".to_string());
        doc.fields.insert("title".to_string(), json!("Hello"));
        doc.created_at = Some("2024-01-01".to_string());
        doc.updated_at = Some("2024-01-02".to_string());

        let result = apply_after_read_inner(&lua, &hooks, &fields, "posts", "find", doc.clone());
        assert_eq!(result.id, "doc1");
        assert_eq!(result.get_str("title"), Some("Hello"));
    }

    // --- has_registered_hooks tests ---

    #[test]
    fn test_has_registered_hooks_empty() {
        let lua = mlua::Lua::new();
        lua.load("_crap_event_hooks = {}").exec().unwrap();
        assert!(!has_registered_hooks(&lua, "after_read"));
    }

    #[test]
    fn test_has_registered_hooks_with_hooks() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            _crap_event_hooks = {
                after_read = { function() end }
            }
        "#).exec().unwrap();
        assert!(has_registered_hooks(&lua, "after_read"));
        assert!(!has_registered_hooks(&lua, "before_change"));
    }

    #[test]
    fn test_has_registered_hooks_no_global() {
        let lua = mlua::Lua::new();
        // _crap_event_hooks is not set at all
        assert!(!has_registered_hooks(&lua, "after_read"));
    }

    // --- HookEvent tests ---

    #[test]
    fn test_hook_event_names() {
        assert_eq!(HookEvent::BeforeValidate.as_str(), "before_validate");
        assert_eq!(HookEvent::BeforeChange.as_str(), "before_change");
        assert_eq!(HookEvent::AfterChange.as_str(), "after_change");
        assert_eq!(HookEvent::BeforeRead.as_str(), "before_read");
        assert_eq!(HookEvent::AfterRead.as_str(), "after_read");
        assert_eq!(HookEvent::BeforeDelete.as_str(), "before_delete");
        assert_eq!(HookEvent::AfterDelete.as_str(), "after_delete");
        assert_eq!(HookEvent::BeforeBroadcast.as_str(), "before_broadcast");
        assert_eq!(HookEvent::BeforeRender.as_str(), "before_render");
    }

    // --- get_hook_refs tests ---

    #[test]
    fn test_get_hook_refs() {
        let hooks = CollectionHooks {
            before_validate: vec!["hooks.validate".to_string()],
            before_change: vec!["hooks.change".to_string()],
            after_change: vec!["hooks.after".to_string()],
            before_read: vec![],
            after_read: vec!["hooks.read".to_string()],
            before_delete: vec![],
            after_delete: vec![],
            before_broadcast: vec!["hooks.broadcast".to_string()],
        };

        assert_eq!(get_hook_refs(&hooks, &HookEvent::BeforeValidate), &["hooks.validate"]);
        assert_eq!(get_hook_refs(&hooks, &HookEvent::BeforeChange), &["hooks.change"]);
        assert_eq!(get_hook_refs(&hooks, &HookEvent::AfterChange), &["hooks.after"]);
        assert!(get_hook_refs(&hooks, &HookEvent::BeforeRead).is_empty());
        assert_eq!(get_hook_refs(&hooks, &HookEvent::AfterRead), &["hooks.read"]);
        assert!(get_hook_refs(&hooks, &HookEvent::BeforeDelete).is_empty());
        assert!(get_hook_refs(&hooks, &HookEvent::AfterDelete).is_empty());
        assert_eq!(get_hook_refs(&hooks, &HookEvent::BeforeBroadcast), &["hooks.broadcast"]);
        assert!(get_hook_refs(&hooks, &HookEvent::BeforeRender).is_empty());
    }

    // --- call_field_hook_ref regression tests ---
    // Regression: field hooks receive (value, context), not (context).
    // A hook that only declared one parameter would bind the field value
    // to its first arg — when the value was nil, it crashed with
    // "attempt to index a nil value".

    #[test]
    fn test_field_hook_receives_value_and_context() {
        let lua = mlua::Lua::new();
        // Hook that returns the value uppercased
        lua.load(r#"
            package.loaded["hooks.upper"] = function(value, context)
                if type(value) == "string" then
                    return value:upper()
                end
                return value
            end
        "#).exec().unwrap();

        let data: HashMap<String, serde_json::Value> =
            [("title".to_string(), json!("hello"))].into_iter().collect();

        let result = call_field_hook_ref(
            &lua, "hooks.upper", json!("hello"),
            "title", "posts", "create", &data,
        ).unwrap();

        assert_eq!(result, json!("HELLO"));
    }

    #[test]
    fn test_field_hook_nil_value_does_not_crash() {
        let lua = mlua::Lua::new();
        // Hook that guards against nil value (correct pattern)
        lua.load(r#"
            package.loaded["hooks.trim"] = function(value, context)
                if type(value) == "string" then
                    return value:match("^%s*(.-)%s*$")
                end
                return value
            end
        "#).exec().unwrap();

        let data: HashMap<String, serde_json::Value> = HashMap::new();

        let result = call_field_hook_ref(
            &lua, "hooks.trim", serde_json::Value::Null,
            "title", "posts", "update", &data,
        ).unwrap();

        assert_eq!(result, serde_json::Value::Null);
    }

    // Regression: field hooks on absent fields (partial update) must not
    // inject Null into the data map. If they do, validation's "skip required
    // for absent fields on update" logic breaks (it checks is_none, not is_null).
    #[test]
    fn test_field_hook_absent_field_not_injected_as_null() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["hooks.noop"] = function(value, context)
                return value
            end
        "#).exec().unwrap();

        let fields = vec![FieldDefinition {
            name: "title".to_string(),
            hooks: FieldHooks {
                before_validate: vec!["hooks.noop".to_string()],
                ..Default::default()
            },
            ..Default::default()
        }];

        // Simulate a partial update: only "content" is in the data, "title" is absent
        let mut data: HashMap<String, serde_json::Value> = HashMap::new();
        data.insert("content".to_string(), json!("updated"));

        run_field_hooks_inner(
            &lua, &fields, &FieldHookEvent::BeforeValidate,
            &mut data, "posts", "update",
        ).unwrap();

        // title must NOT appear in data — it was absent and hook returned null
        assert!(!data.contains_key("title"),
            "absent field should not be injected into data by field hooks");
        // content must be untouched
        assert_eq!(data.get("content"), Some(&json!("updated")));
    }

    // Verify that a hook generating a value for an absent field DOES insert it
    // (e.g. auto_slug on create where slug is absent but hook produces a value)
    #[test]
    fn test_field_hook_absent_field_inserted_when_hook_produces_value() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["hooks.default_val"] = function(value, context)
                if value == nil then
                    return "generated"
                end
                return value
            end
        "#).exec().unwrap();

        let fields = vec![FieldDefinition {
            name: "slug".to_string(),
            hooks: FieldHooks {
                before_validate: vec!["hooks.default_val".to_string()],
                ..Default::default()
            },
            ..Default::default()
        }];

        let mut data: HashMap<String, serde_json::Value> = HashMap::new();
        data.insert("title".to_string(), json!("Hello"));

        run_field_hooks_inner(
            &lua, &fields, &FieldHookEvent::BeforeValidate,
            &mut data, "posts", "create",
        ).unwrap();

        // Hook produced a non-null value for absent field — it should be inserted
        assert_eq!(data.get("slug"), Some(&json!("generated")));
    }

    #[test]
    fn test_field_hook_context_has_data_and_metadata() {
        let lua = mlua::Lua::new();
        // Hook that reads context fields and returns them as proof
        lua.load(r#"
            package.loaded["hooks.inspect_ctx"] = function(value, context)
                return context.collection .. ":" .. context.field_name .. ":" .. context.operation
            end
        "#).exec().unwrap();

        let data: HashMap<String, serde_json::Value> =
            [("title".to_string(), json!("hello"))].into_iter().collect();

        let result = call_field_hook_ref(
            &lua, "hooks.inspect_ctx", json!("hello"),
            "title", "posts", "create", &data,
        ).unwrap();

        assert_eq!(result, json!("posts:title:create"));
    }
}
