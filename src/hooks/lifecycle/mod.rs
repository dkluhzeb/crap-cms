//! Hook execution engine: runs field, collection, and registered hooks within transactions.

pub mod crud;
pub mod access;

use anyhow::{Context, Result};
use mlua::{Lua, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};

use crate::config::CrapConfig;
use crate::core::collection::{CollectionHooks, LiveSetting};
use crate::core::event::{EventBus, EventTarget, EventOperation, EventUser};
use crate::core::Document;
use crate::core::SharedRegistry;
use crate::core::field::{FieldDefinition, FieldHooks, FieldType};
use crate::core::validate::{FieldError, ValidationError};
use crate::db::query::{self, AccessResult};

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

/// Context passed to hook functions.
#[derive(Debug, Clone)]
pub struct HookContext {
    pub collection: String,
    pub operation: String,
    pub data: HashMap<String, serde_json::Value>,
    pub locale: Option<String>,
    /// Whether this operation is a draft save (`true` = draft, `false`/`None` = publish).
    pub draft: Option<bool>,
    /// Request-scoped shared table that flows from before_validate through after_change.
    /// Hooks can read/write this to share state within one request lifecycle.
    /// Only JSON-compatible values survive (no functions, userdata, etc.).
    pub context: HashMap<String, serde_json::Value>,
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

/// Pool of Lua VMs for concurrent hook execution.
struct VmPool {
    vms: Mutex<Vec<Lua>>,
    available: Condvar,
}

impl VmPool {
    fn new(vms: Vec<Lua>) -> Self {
        VmPool {
            vms: Mutex::new(vms),
            available: Condvar::new(),
        }
    }

    /// Acquire a VM from the pool, blocking until one is available.
    fn acquire(&self) -> std::result::Result<VmGuard<'_>, String> {
        let mut pool = self.vms.lock()
            .map_err(|e| format!("VM pool lock poisoned: {}", e))?;
        loop {
            if let Some(vm) = pool.pop() {
                return Ok(VmGuard { pool: self, vm: Some(vm) });
            }
            pool = self.available.wait(pool)
                .map_err(|e| format!("VM pool condvar wait failed: {}", e))?;
        }
    }
}

/// RAII guard that returns a VM to the pool on drop.
struct VmGuard<'a> {
    pool: &'a VmPool,
    vm: Option<Lua>,
}

impl std::ops::Deref for VmGuard<'_> {
    type Target = Lua;
    fn deref(&self) -> &Lua {
        self.vm.as_ref().unwrap()
    }
}

impl Drop for VmGuard<'_> {
    fn drop(&mut self) {
        if let Some(vm) = self.vm.take() {
            if let Ok(mut pool) = self.pool.vms.lock() {
                pool.push(vm);
                self.pool.available.notify_one();
            }
        }
    }
}

/// Thread-safe hook runner with a pool of Lua VMs for concurrent execution.
#[derive(Clone)]
pub struct HookRunner {
    pool: Arc<VmPool>,
}

/// Create and fully initialize a single Lua VM with package paths, API, CRUD functions,
/// collection/global/job loading, and init.lua execution.
fn create_lua_vm(
    config_dir: &Path,
    registry: SharedRegistry,
    config: &CrapConfig,
) -> Result<Lua> {
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
    crate::hooks::api::register_api(&lua, registry.clone(), config_dir, config)?;

    // Register CRUD functions on crap.collections (find, find_by_id, create, update, delete).
    // These read the active transaction from Lua app_data when called inside hooks.
    register_crud_functions(&lua, registry, &config.locale, config.hooks.max_depth)?;

    // Initialize hook depth tracking
    lua.set_app_data(HookDepth(0));
    lua.set_app_data(MaxHookDepth(config.hooks.max_depth));

    // Auto-load collections/*.lua, globals/*.lua, and jobs/*.lua
    let collections_dir = config_dir.join("collections");
    if collections_dir.exists() {
        crate::hooks::load_lua_dir(&lua, &collections_dir, "collection")?;
    }
    let globals_dir = config_dir.join("globals");
    if globals_dir.exists() {
        crate::hooks::load_lua_dir(&lua, &globals_dir, "global")?;
    }
    let jobs_dir = config_dir.join("jobs");
    if jobs_dir.exists() {
        crate::hooks::load_lua_dir(&lua, &jobs_dir, "job")?;
    }

    // Execute init.lua so crap.hooks.register() calls take effect in this VM
    let init_path = config_dir.join("init.lua");
    if init_path.exists() {
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
        for _ in 0..pool_size {
            vms.push(create_lua_vm(config_dir, registry.clone(), config)?);
        }

        Ok(Self {
            pool: Arc::new(VmPool::new(vms)),
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

        // If no collection-level or registered hooks, pass through
        let has_registered = {
            let lua = self.pool.acquire()
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            has_registered_hooks(&lua, "before_broadcast")
        };

        if hook_refs.is_empty() && !has_registered {
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

        // run_hooks handles both collection-level hook refs and global registered hooks.
        // We need to check if any hook returns false/nil to suppress.
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
        let func = resolve_hook_function(&lua, func_ref).ok()?;
        let data_lua = crate::hooks::api::json_to_lua(&lua, form_data).ok()?;
        match func.call::<Value>(data_lua) {
            Ok(Value::Boolean(b)) => Some(DisplayConditionResult::Bool(b)),
            Ok(val @ Value::Table(_)) => {
                let json = crate::hooks::api::lua_to_json(&lua, &val).ok()?;
                let visible = evaluate_condition_table(&json, form_data);
                Some(DisplayConditionResult::Table { condition: json, visible })
            }
            _ => None, // error or nil → show field (safe default)
        }
    }

    /// Run `before_render` hooks on the template context.
    /// Global registered `before_render` hooks receive the full template context as a
    /// Lua table and return the (potentially modified) context. No CRUD access.
    /// On error: logs warning, returns original context unmodified.
    pub fn run_before_render(&self, mut context: serde_json::Value) -> serde_json::Value {
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

// ── Extracted inner functions (callable from both HookRunner and CRUD closures) ──

/// Inner implementation of `validate_fields` — operates on a locked `&Lua`.
/// Used by both `HookRunner::validate_fields` and Lua CRUD closures.
pub(super) fn validate_fields_inner(
    lua: &Lua,
    fields: &[FieldDefinition],
    data: &HashMap<String, serde_json::Value>,
    conn: &rusqlite::Connection,
    table: &str,
    exclude_id: Option<&str>,
    is_draft: bool,
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
        // Skip required checks entirely for draft saves
        // On update (exclude_id set): skip if field not in data (partial update, keep existing)
        let is_update = exclude_id.is_some();
        if field.required && !is_draft && field.field_type != FieldType::Checkbox
            && !(is_update && value.is_none())
        {
            if !field.has_parent_column() {
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

        // Validate Group sub-fields (stored as group__subfield keys at top level)
        if field.field_type == FieldType::Group && !is_draft {
            for gsf in &field.fields {
                let key = format!("{}__{}", field.name, gsf.name);
                let gv = data.get(&key);
                let g_empty = match gv {
                    None => true,
                    Some(serde_json::Value::Null) => true,
                    Some(serde_json::Value::String(s)) => s.is_empty(),
                    _ => false,
                };
                if gsf.required && g_empty && gsf.field_type != FieldType::Checkbox {
                    errors.push(FieldError {
                        field: key,
                        message: format!("{} is required", gsf.name),
                    });
                }
            }
        }

        // min_rows / max_rows validation for Array, Blocks, and has-many Relationship
        if !is_draft && (field.min_rows.is_some() || field.max_rows.is_some()) {
            let row_count = match value {
                Some(serde_json::Value::Array(arr)) => arr.len(),
                _ => 0,
            };
            if let Some(min) = field.min_rows {
                if row_count < min {
                    errors.push(FieldError {
                        field: field.name.clone(),
                        message: format!("{} requires at least {} item(s)", field.name, min),
                    });
                }
            }
            if let Some(max) = field.max_rows {
                if row_count > max {
                    errors.push(FieldError {
                        field: field.name.clone(),
                        message: format!("{} allows at most {} item(s)", field.name, max),
                    });
                }
            }
        }

        // Validate sub-fields within Array/Blocks rows
        if !is_draft && matches!(field.field_type, FieldType::Array | FieldType::Blocks) {
            if let Some(serde_json::Value::Array(rows)) = value {
                for (idx, row) in rows.iter().enumerate() {
                    let row_obj = match row.as_object() {
                        Some(obj) => obj,
                        None => continue,
                    };

                    let sub_fields: &[FieldDefinition] = if field.field_type == FieldType::Blocks {
                        let block_type = row_obj.get("_block_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        match field.blocks.iter().find(|b| b.block_type == block_type) {
                            Some(bd) => &bd.fields,
                            None => continue,
                        }
                    } else {
                        &field.fields
                    };

                    validate_sub_fields_inner(
                        lua, sub_fields, row_obj, &field.name, idx, table, &mut errors,
                    );
                }
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

        // Date format validation (only if non-empty)
        if field.field_type == FieldType::Date && !is_empty {
            if let Some(serde_json::Value::String(s)) = value {
                if !is_valid_date_format(s) {
                    errors.push(FieldError {
                        field: field.name.clone(),
                        message: format!("{} is not a valid date format", field.name),
                    });
                }
            }
        }

        // Custom validate function (Lua)
        if let Some(ref validate_ref) = field.validate {
            if let Some(val) = value {
                match run_validate_function_inner(lua, validate_ref, val, data, table, &field.name) {
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

/// Validate sub-fields within a single array/blocks row (inner, no mutex).
fn validate_sub_fields_inner(
    lua: &Lua,
    sub_fields: &[FieldDefinition],
    row_obj: &serde_json::Map<String, serde_json::Value>,
    parent_name: &str,
    idx: usize,
    table: &str,
    errors: &mut Vec<FieldError>,
) {
    let row_data: HashMap<String, serde_json::Value> = row_obj.iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    for sf in sub_fields {
        let sf_value = row_obj.get(&sf.name);
        let sf_empty = match sf_value {
            None => true,
            Some(serde_json::Value::Null) => true,
            Some(serde_json::Value::String(s)) => s.is_empty(),
            _ => false,
        };
        let qualified_name = format!("{}[{}][{}]", parent_name, idx, sf.name);

        if sf.required && sf_empty && sf.field_type != FieldType::Checkbox {
            errors.push(FieldError {
                field: qualified_name.clone(),
                message: format!("{} is required", sf.name),
            });
        }

        if sf.field_type == FieldType::Date && !sf_empty {
            if let Some(serde_json::Value::String(s)) = sf_value {
                if !is_valid_date_format(s) {
                    errors.push(FieldError {
                        field: qualified_name.clone(),
                        message: format!("{} is not a valid date format", sf.name),
                    });
                }
            }
        }

        if let Some(ref validate_ref) = sf.validate {
            if let Some(val) = sf_value {
                match run_validate_function_inner(lua, validate_ref, val, &row_data, table, &sf.name) {
                    Ok(Some(err_msg)) => {
                        errors.push(FieldError {
                            field: qualified_name.clone(),
                            message: err_msg,
                        });
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!("Validate function '{}' error: {}", validate_ref, e);
                    }
                }
            }
        }

        if matches!(sf.field_type, FieldType::Array | FieldType::Blocks) {
            if let Some(serde_json::Value::Array(nested_rows)) = sf_value {
                let nested_parent = format!("{}[{}][{}]", parent_name, idx, sf.name);
                for (nested_idx, nested_row) in nested_rows.iter().enumerate() {
                    if let Some(nested_obj) = nested_row.as_object() {
                        let nested_sub_fields: &[FieldDefinition] = if sf.field_type == FieldType::Blocks {
                            let bt = nested_obj.get("_block_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            match sf.blocks.iter().find(|b| b.block_type == bt) {
                                Some(bd) => &bd.fields,
                                None => continue,
                            }
                        } else {
                            &sf.fields
                        };
                        validate_sub_fields_inner(
                            lua, nested_sub_fields, nested_obj, &nested_parent, nested_idx, table, errors,
                        );
                    }
                }
            }
        }

        if sf.field_type == FieldType::Group {
            for gsf in &sf.fields {
                let group_key = format!("{}__{}", sf.name, gsf.name);
                let gv = row_obj.get(&group_key);
                let g_empty = match gv {
                    None => true,
                    Some(serde_json::Value::Null) => true,
                    Some(serde_json::Value::String(s)) => s.is_empty(),
                    _ => false,
                };
                let g_qualified = format!("{}[{}][{}]", parent_name, idx, group_key);

                if gsf.required && g_empty && gsf.field_type != FieldType::Checkbox {
                    errors.push(FieldError {
                        field: g_qualified.clone(),
                        message: format!("{} is required", gsf.name),
                    });
                }

                if gsf.field_type == FieldType::Date && !g_empty {
                    if let Some(serde_json::Value::String(s)) = gv {
                        if !is_valid_date_format(s) {
                            errors.push(FieldError {
                                field: g_qualified.clone(),
                                message: format!("{} is not a valid date format", gsf.name),
                            });
                        }
                    }
                }

                if let Some(ref validate_ref) = gsf.validate {
                    if let Some(val) = gv {
                        match run_validate_function_inner(lua, validate_ref, val, &row_data, table, &gsf.name) {
                            Ok(Some(err_msg)) => {
                                errors.push(FieldError {
                                    field: g_qualified,
                                    message: err_msg,
                                });
                            }
                            Ok(None) => {}
                            Err(e) => {
                                tracing::warn!("Validate function '{}' error: {}", validate_ref, e);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Inner implementation of `run_validate_function` — operates on a locked `&Lua`.
pub(super) fn run_validate_function_inner(
    lua: &Lua,
    func_ref: &str,
    value: &serde_json::Value,
    data: &HashMap<String, serde_json::Value>,
    collection: &str,
    field_name: &str,
) -> Result<Option<String>> {
    let func = resolve_hook_function(lua, func_ref)?;
    let lua_value = crate::hooks::api::json_to_lua(lua, value)?;
    let ctx_table = lua.create_table()?;
    ctx_table.set("collection", collection)?;
    ctx_table.set("field_name", field_name)?;
    let data_table = lua.create_table()?;
    for (k, v) in data {
        data_table.set(k.as_str(), crate::hooks::api::json_to_lua(lua, v)?)?;
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

/// Build a Lua table from a HookContext (shared by all context table builders).
fn context_to_lua_table(lua: &Lua, context: &HookContext) -> mlua::Result<mlua::Table> {
    let ctx_table = lua.create_table()?;
    ctx_table.set("collection", context.collection.as_str())?;
    ctx_table.set("operation", context.operation.as_str())?;
    if let Some(ref locale) = context.locale {
        ctx_table.set("locale", locale.as_str())?;
    }
    if let Some(draft) = context.draft {
        ctx_table.set("draft", draft)?;
    }
    let data_table = lua.create_table()?;
    for (k, v) in &context.data {
        data_table.set(k.as_str(), crate::hooks::api::json_to_lua(lua, v)?)?;
    }
    ctx_table.set("data", data_table)?;

    // Request-scoped shared context table
    let context_table = lua.create_table()?;
    for (k, v) in &context.context {
        context_table.set(k.as_str(), crate::hooks::api::json_to_lua(lua, v)?)?;
    }
    ctx_table.set("context", context_table)?;

    // Expose current hook depth so hooks can make manual decisions
    let depth = lua.app_data_ref::<HookDepth>().map(|d| d.0).unwrap_or(0);
    ctx_table.set("hook_depth", depth)?;

    Ok(ctx_table)
}

/// Convert hook context data (JSON values) back to string map for query functions.
/// Only includes fields that have parent table columns (skips array/has-many).
/// Group fields are flattened from `{ "seo": { "meta_title": "X" } }` to
/// `{ "seo__meta_title": "X" }` so `query::create/update` can find them.
pub fn hook_ctx_to_string_map(
    ctx: &HookContext,
    fields: &[crate::core::field::FieldDefinition],
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (k, v) in &ctx.data {
        // Check if this key is a group field that needs flattening
        let is_group = fields.iter().any(|f| {
            f.name == *k && f.field_type == crate::core::field::FieldType::Group
        });
        if is_group {
            if let Some(obj) = v.as_object() {
                for (sub_key, sub_val) in obj {
                    let flat_key = format!("{}__{}", k, sub_key);
                    let flat_val = match sub_val {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    map.insert(flat_key, flat_val);
                }
                continue;
            }
            // If the value is already a string (e.g. from form data), fall through
        }
        map.insert(k.clone(), match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        });
    }
    map
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

/// Read the `context` table from a returned Lua hook table, merging into the existing context.
fn read_context_back(lua: &Lua, tbl: &mlua::Table, existing: &mut HashMap<String, serde_json::Value>) {
    if let Ok(context_tbl) = tbl.get::<mlua::Table>("context") {
        existing.clear();
        for pair in context_tbl.pairs::<String, Value>() {
            if let Ok((k, v)) = pair {
                if let Ok(json_val) = crate::hooks::api::lua_to_json(lua, &v) {
                    existing.insert(k, json_val);
                }
            }
        }
    }
}

/// Check if a string is a recognized date format for the date field type.
/// Accepts: YYYY-MM-DD, YYYY-MM-DDTHH:MM, YYYY-MM-DDTHH:MM:SS, full ISO 8601/RFC 3339,
/// HH:MM (time only), HH:MM:SS, YYYY-MM (month only).
fn is_valid_date_format(value: &str) -> bool {
    use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime};

    // Time only: HH:MM or HH:MM:SS
    if value.len() <= 8 && value.contains(':') && !value.contains('T') {
        let parts: Vec<&str> = value.split(':').collect();
        if parts.len() >= 2 {
            return parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()));
        }
    }

    // Month only: YYYY-MM
    if value.len() == 7 && value.as_bytes().get(4) == Some(&b'-') && !value.contains('T') {
        let parts: Vec<&str> = value.split('-').collect();
        if parts.len() == 2 {
            return parts[0].len() == 4
                && parts[0].chars().all(|c| c.is_ascii_digit())
                && parts[1].len() == 2
                && parts[1].chars().all(|c| c.is_ascii_digit());
        }
    }

    // Full RFC 3339
    if DateTime::<FixedOffset>::parse_from_rfc3339(value).is_ok() {
        return true;
    }

    // Date only: YYYY-MM-DD
    if value.len() == 10 {
        return NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok();
    }

    // datetime-local: YYYY-MM-DDTHH:MM
    if value.len() == 16 && value.contains('T') {
        return NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M").is_ok();
    }

    // YYYY-MM-DDTHH:MM:SS (no timezone)
    if value.len() == 19 && value.contains('T') {
        return NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").is_ok();
    }

    false
}

/// Evaluate a condition table (JSON) against form data.
/// A single condition object has `{ field, equals|not_equals|in|not_in|is_truthy|is_falsy }`.
/// An array of conditions means AND (all must be true).
pub fn evaluate_condition_table(
    condition: &serde_json::Value,
    data: &serde_json::Value,
) -> bool {
    match condition {
        serde_json::Value::Array(arr) => arr.iter().all(|c| evaluate_condition_table(c, data)),
        serde_json::Value::Object(obj) => {
            let field_name = obj.get("field").and_then(|v| v.as_str()).unwrap_or("");
            let field_val = data.get(field_name).unwrap_or(&serde_json::Value::Null);

            if let Some(eq) = obj.get("equals") {
                return field_val == eq;
            }
            if let Some(neq) = obj.get("not_equals") {
                return field_val != neq;
            }
            if let Some(serde_json::Value::Array(list)) = obj.get("in") {
                return list.contains(field_val);
            }
            if let Some(serde_json::Value::Array(list)) = obj.get("not_in") {
                return !list.contains(field_val);
            }
            if obj.get("is_truthy").and_then(|v| v.as_bool()).unwrap_or(false) {
                return condition_is_truthy(field_val);
            }
            if obj.get("is_falsy").and_then(|v| v.as_bool()).unwrap_or(false) {
                return !condition_is_truthy(field_val);
            }
            true // unknown operator → show
        }
        _ => true,
    }
}

/// Check if a JSON value is "truthy" for display condition evaluation.
fn condition_is_truthy(val: &serde_json::Value) -> bool {
    match val {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Number(_) => true,
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
    }
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

    // --- is_valid_date_format tests ---

    #[test]
    fn test_valid_date_format_date_only() {
        assert!(is_valid_date_format("2024-01-15"));
        assert!(is_valid_date_format("2000-12-31"));
        assert!(is_valid_date_format("1999-06-01"));
    }

    #[test]
    fn test_valid_date_format_datetime_local() {
        assert!(is_valid_date_format("2024-01-15T10:30"));
        assert!(is_valid_date_format("2024-12-31T23:59"));
    }

    #[test]
    fn test_valid_date_format_datetime_seconds() {
        assert!(is_valid_date_format("2024-01-15T10:30:45"));
        assert!(is_valid_date_format("2024-12-31T23:59:59"));
    }

    #[test]
    fn test_valid_date_format_rfc3339() {
        assert!(is_valid_date_format("2024-01-15T10:30:00+00:00"));
        assert!(is_valid_date_format("2024-01-15T10:30:00Z"));
        assert!(is_valid_date_format("2024-01-15T10:30:00-05:00"));
    }

    #[test]
    fn test_valid_date_format_time_only() {
        assert!(is_valid_date_format("10:30"));
        assert!(is_valid_date_format("23:59"));
        assert!(is_valid_date_format("00:00"));
        assert!(is_valid_date_format("10:30:45"));
    }

    #[test]
    fn test_valid_date_format_month_only() {
        assert!(is_valid_date_format("2024-01"));
        assert!(is_valid_date_format("2024-12"));
        assert!(is_valid_date_format("1999-06"));
    }

    #[test]
    fn test_invalid_date_format() {
        assert!(!is_valid_date_format(""));
        assert!(!is_valid_date_format("not-a-date"));
        assert!(!is_valid_date_format("2024"));
        assert!(!is_valid_date_format("2024-1-1"));
        assert!(!is_valid_date_format("01/15/2024"));
        assert!(!is_valid_date_format("2024-13-01")); // invalid month
        assert!(!is_valid_date_format("2024-01-32")); // invalid day
    }

    // --- condition_is_truthy tests ---

    #[test]
    fn test_condition_is_truthy_null() {
        assert!(!condition_is_truthy(&json!(null)));
    }

    #[test]
    fn test_condition_is_truthy_bool() {
        assert!(condition_is_truthy(&json!(true)));
        assert!(!condition_is_truthy(&json!(false)));
    }

    #[test]
    fn test_condition_is_truthy_string() {
        assert!(condition_is_truthy(&json!("hello")));
        assert!(!condition_is_truthy(&json!("")));
    }

    #[test]
    fn test_condition_is_truthy_number() {
        assert!(condition_is_truthy(&json!(0)));
        assert!(condition_is_truthy(&json!(42)));
        assert!(condition_is_truthy(&json!(-1)));
    }

    #[test]
    fn test_condition_is_truthy_array() {
        assert!(condition_is_truthy(&json!([1, 2])));
        assert!(!condition_is_truthy(&json!([])));
    }

    #[test]
    fn test_condition_is_truthy_object() {
        assert!(condition_is_truthy(&json!({"key": "value"})));
        assert!(!condition_is_truthy(&json!({})));
    }

    // --- evaluate_condition_table tests ---

    #[test]
    fn test_condition_equals() {
        let data = json!({"status": "published"});
        let cond = json!({"field": "status", "equals": "published"});
        assert!(evaluate_condition_table(&cond, &data));

        let cond_miss = json!({"field": "status", "equals": "draft"});
        assert!(!evaluate_condition_table(&cond_miss, &data));
    }

    #[test]
    fn test_condition_not_equals() {
        let data = json!({"status": "published"});
        let cond = json!({"field": "status", "not_equals": "draft"});
        assert!(evaluate_condition_table(&cond, &data));

        let cond_miss = json!({"field": "status", "not_equals": "published"});
        assert!(!evaluate_condition_table(&cond_miss, &data));
    }

    #[test]
    fn test_condition_in() {
        let data = json!({"category": "tech"});
        let cond = json!({"field": "category", "in": ["tech", "science"]});
        assert!(evaluate_condition_table(&cond, &data));

        let cond_miss = json!({"field": "category", "in": ["art", "music"]});
        assert!(!evaluate_condition_table(&cond_miss, &data));
    }

    #[test]
    fn test_condition_not_in() {
        let data = json!({"category": "tech"});
        let cond = json!({"field": "category", "not_in": ["art", "music"]});
        assert!(evaluate_condition_table(&cond, &data));

        let cond_miss = json!({"field": "category", "not_in": ["tech", "science"]});
        assert!(!evaluate_condition_table(&cond_miss, &data));
    }

    #[test]
    fn test_condition_is_truthy_op() {
        let data = json!({"featured": true});
        let cond = json!({"field": "featured", "is_truthy": true});
        assert!(evaluate_condition_table(&cond, &data));

        let data_false = json!({"featured": false});
        assert!(!evaluate_condition_table(&cond, &data_false));
    }

    #[test]
    fn test_condition_is_falsy_op() {
        let data = json!({"featured": false});
        let cond = json!({"field": "featured", "is_falsy": true});
        assert!(evaluate_condition_table(&cond, &data));

        let data_true = json!({"featured": true});
        assert!(!evaluate_condition_table(&cond, &data_true));
    }

    #[test]
    fn test_condition_array_and() {
        let data = json!({"status": "published", "featured": true});
        let cond = json!([
            {"field": "status", "equals": "published"},
            {"field": "featured", "is_truthy": true}
        ]);
        assert!(evaluate_condition_table(&cond, &data));

        let data_fail = json!({"status": "draft", "featured": true});
        assert!(!evaluate_condition_table(&cond, &data_fail));
    }

    #[test]
    fn test_condition_missing_field() {
        let data = json!({"status": "published"});
        let cond = json!({"field": "nonexistent", "equals": "something"});
        assert!(!evaluate_condition_table(&cond, &data));
    }

    #[test]
    fn test_condition_unknown_operator_shows() {
        let data = json!({"status": "published"});
        let cond = json!({"field": "status"});
        // Unknown operator → show (returns true)
        assert!(evaluate_condition_table(&cond, &data));
    }

    #[test]
    fn test_condition_non_object_non_array_shows() {
        let data = json!({"status": "published"});
        // Non-object, non-array → true
        assert!(evaluate_condition_table(&json!("string"), &data));
        assert!(evaluate_condition_table(&json!(42), &data));
        assert!(evaluate_condition_table(&json!(null), &data));
    }

    // --- validate_fields_inner tests ---

    #[test]
    fn test_validate_required_field_empty_string() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            required: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.errors.len(), 1);
        assert!(err.errors[0].message.contains("required"));
    }

    #[test]
    fn test_validate_required_field_null() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            required: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!(null));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_required_skipped_for_drafts() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            required: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, true);
        assert!(result.is_ok(), "Drafts should skip required checks");
    }

    #[test]
    fn test_validate_required_join_field_empty_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Relationship,
            required: true,
            relationship: Some(crate::core::field::RelationshipConfig {
                collection: "tags".to_string(),
                has_many: true,
                max_depth: None,
            }),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!([]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("required"));
    }

    #[test]
    fn test_validate_required_join_field_non_empty_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Relationship,
            required: true,
            relationship: Some(crate::core::field::RelationshipConfig {
                collection: "tags".to_string(),
                has_many: true,
                max_depth: None,
            }),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(["t1", "t2"]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_group_subfield_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "seo".to_string(),
            field_type: FieldType::Group,
            fields: vec![FieldDefinition {
                name: "title".to_string(),
                required: true,
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_min_rows() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            min_rows: Some(2),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"label": "one"}]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at least 2"));
    }

    #[test]
    fn test_validate_max_rows() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            max_rows: Some(1),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"a": 1}, {"a": 2}]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at most 1"));
    }

    #[test]
    fn test_validate_array_sub_field_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            fields: vec![FieldDefinition {
                name: "label".to_string(),
                required: true,
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"label": ""}]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.errors[0].field.contains("items[0][label]"));
    }

    #[test]
    fn test_validate_blocks_sub_field_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "content".to_string(),
            field_type: FieldType::Blocks,
            blocks: vec![crate::core::field::BlockDefinition {
                block_type: "text".to_string(),
                fields: vec![FieldDefinition {
                    name: "body".to_string(),
                    required: true,
                    ..Default::default()
                }],
                label: None,
                label_field: None,
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("content".to_string(), json!([{"_block_type": "text", "body": ""}]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].field.contains("content[0][body]"));
    }

    #[test]
    fn test_validate_date_format_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, d TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "d".to_string(),
            field_type: FieldType::Date,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("d".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_date_format_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, d TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "d".to_string(),
            field_type: FieldType::Date,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("d".to_string(), json!("2024-01-15"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_custom_validate_function_returns_error() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = {
                validate_test = function(value, ctx)
                    if value == "bad" then
                        return "value cannot be bad"
                    end
                    return true
                end
            }
        "#).exec().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            validate: Some("validators.validate_test".to_string()),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("bad"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("cannot be bad"));
    }

    #[test]
    fn test_validate_custom_validate_function_returns_false() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_fail = function(value, ctx)
                return false
            end
        "#).exec().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            validate: Some("validators.validate_fail".to_string()),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("anything"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].message, "validation failed");
    }

    #[test]
    fn test_validate_custom_validate_function_returns_true() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_ok = function(value, ctx)
                return true
            end
        "#).exec().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            validate: Some("validators.validate_ok".to_string()),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("good"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_unique_check() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT);
             INSERT INTO test (id, email) VALUES ('existing', 'taken@test.com');"
        ).unwrap();
        let fields = vec![FieldDefinition {
            name: "email".to_string(),
            unique: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("taken@test.com"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("unique"));
    }

    #[test]
    fn test_validate_unique_check_excludes_self() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT);
             INSERT INTO test (id, email) VALUES ('self', 'me@test.com');"
        ).unwrap();
        let fields = vec![FieldDefinition {
            name: "email".to_string(),
            unique: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("me@test.com"));
        // exclude_id = "self" means we're updating ourselves, so this is fine
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", Some("self"), false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_required_skipped_on_update_absent_field() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            required: true,
            ..Default::default()
        }];
        // On update (exclude_id set), absent field = partial update, should not fail
        let data = HashMap::new();
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", Some("p1"), false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_checkbox_not_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, active INTEGER)").unwrap();
        let fields = vec![FieldDefinition {
            name: "active".to_string(),
            field_type: FieldType::Checkbox,
            required: true,
            ..Default::default()
        }];
        // Checkbox absent = false, which is valid even when required
        let data = HashMap::new();
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok());
    }

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

    // --- run_validate_function_inner tests ---

    #[test]
    fn test_run_validate_function_nil_means_valid() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = {
                validate_nil = function(value, ctx)
                    return nil
                end
            }
        "#).exec().unwrap();
        let data = HashMap::new();
        let result = run_validate_function_inner(&lua, "validators.validate_nil", &json!("test"), &data, "test", "name").unwrap();
        assert!(result.is_none());
    }

    // --- context_to_lua_table tests ---

    #[test]
    fn test_context_to_lua_table_with_locale_and_draft() {
        let lua = mlua::Lua::new();
        lua.set_app_data(HookDepth(3));
        let mut ctx_map = HashMap::new();
        ctx_map.insert("request_id".to_string(), json!("abc-123"));
        let ctx = HookContext {
            collection: "posts".to_string(),
            operation: "create".to_string(),
            data: {
                let mut d = HashMap::new();
                d.insert("title".to_string(), json!("Hello"));
                d
            },
            locale: Some("en".to_string()),
            draft: Some(true),
            context: ctx_map,
        };
        let tbl = context_to_lua_table(&lua, &ctx).unwrap();
        let collection: String = tbl.get("collection").unwrap();
        assert_eq!(collection, "posts");
        let locale: String = tbl.get("locale").unwrap();
        assert_eq!(locale, "en");
        let draft: bool = tbl.get("draft").unwrap();
        assert!(draft);
        let depth: u32 = tbl.get("hook_depth").unwrap();
        assert_eq!(depth, 3);
        let context_tbl: mlua::Table = tbl.get("context").unwrap();
        let req_id: String = context_tbl.get("request_id").unwrap();
        assert_eq!(req_id, "abc-123");
    }

    // --- read_context_back tests ---

    #[test]
    fn test_read_context_back() {
        let lua = mlua::Lua::new();
        let tbl = lua.create_table().unwrap();
        let context_tbl = lua.create_table().unwrap();
        context_tbl.set("key1", "value1").unwrap();
        context_tbl.set("key2", 42).unwrap();
        tbl.set("context", context_tbl).unwrap();

        let mut existing = HashMap::new();
        existing.insert("old_key".to_string(), json!("old_value"));
        read_context_back(&lua, &tbl, &mut existing);

        assert!(!existing.contains_key("old_key"), "old entries should be cleared");
        assert_eq!(existing.get("key1"), Some(&json!("value1")));
        assert_eq!(existing.get("key2"), Some(&json!(42)));
    }

    #[test]
    fn test_read_context_back_no_context_table() {
        let lua = mlua::Lua::new();
        let tbl = lua.create_table().unwrap();
        // No "context" key in the table

        let mut existing = HashMap::new();
        existing.insert("old_key".to_string(), json!("old_value"));
        read_context_back(&lua, &tbl, &mut existing);

        // Should not change existing since there is no context table
        assert!(existing.contains_key("old_key"));
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

    // --- nested validation in sub-fields ---

    #[test]
    fn test_validate_nested_array_in_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "outer".to_string(),
            field_type: FieldType::Array,
            fields: vec![FieldDefinition {
                name: "inner".to_string(),
                field_type: FieldType::Array,
                fields: vec![FieldDefinition {
                    name: "value".to_string(),
                    required: true,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("outer".to_string(), json!([
            {"inner": [{"value": ""}]}
        ]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.errors[0].field.contains("outer[0][inner][0][value]"));
    }

    #[test]
    fn test_validate_group_inside_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            fields: vec![FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![FieldDefinition {
                    name: "title".to_string(),
                    required: true,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([
            {"meta__title": ""}
        ]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.errors[0].field.contains("items[0][meta__title]"));
    }

    #[test]
    fn test_validate_date_inside_array_subfield() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "events".to_string(),
            field_type: FieldType::Array,
            fields: vec![FieldDefinition {
                name: "date".to_string(),
                field_type: FieldType::Date,
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("events".to_string(), json!([
            {"date": "not-a-date"}
        ]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_custom_validate_in_array_subfield() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_sub = function(value, ctx)
                if value == "invalid" then
                    return "sub-field invalid"
                end
                return true
            end
        "#).exec().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            fields: vec![FieldDefinition {
                name: "val".to_string(),
                validate: Some("validators.validate_sub".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([
            {"val": "invalid"}
        ]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("sub-field invalid"));
    }

    #[test]
    fn test_validate_date_in_group_inside_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            fields: vec![FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![FieldDefinition {
                    name: "publish_date".to_string(),
                    field_type: FieldType::Date,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([
            {"meta__publish_date": "bad-date"}
        ]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_custom_function_in_group_inside_array() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_group_sub = function(value, ctx)
                return "group validation error"
            end
        "#).exec().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            fields: vec![FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![FieldDefinition {
                    name: "slug".to_string(),
                    validate: Some("validators.validate_group_sub".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([
            {"meta__slug": "test-slug"}
        ]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("group validation error"));
    }

    // --- hook_ctx_to_string_map tests ---

    #[test]
    fn test_hook_ctx_to_string_map_simple() {
        let mut data = HashMap::new();
        data.insert("title".to_string(), json!("Hello World"));
        data.insert("count".to_string(), json!(42));
        data.insert("active".to_string(), json!(true));

        let ctx = HookContext {
            collection: "posts".to_string(),
            operation: "create".to_string(),
            data,
            locale: None,
            draft: None,
            context: HashMap::new(),
        };

        let fields = vec![
            FieldDefinition {
                name: "title".to_string(),
                field_type: FieldType::Text,
                ..Default::default()
            },
            FieldDefinition {
                name: "count".to_string(),
                field_type: FieldType::Number,
                ..Default::default()
            },
            FieldDefinition {
                name: "active".to_string(),
                field_type: FieldType::Checkbox,
                ..Default::default()
            },
        ];

        let map = hook_ctx_to_string_map(&ctx, &fields);
        assert_eq!(map.get("title").unwrap(), "Hello World");
        assert_eq!(map.get("count").unwrap(), "42");
        assert_eq!(map.get("active").unwrap(), "true");
    }

    #[test]
    fn test_hook_ctx_to_string_map_group_flattening() {
        let mut data = HashMap::new();
        data.insert("seo".to_string(), json!({
            "meta_title": "My Title",
            "meta_description": "My Description"
        }));
        data.insert("title".to_string(), json!("Hello"));

        let ctx = HookContext {
            collection: "posts".to_string(),
            operation: "create".to_string(),
            data,
            locale: None,
            draft: None,
            context: HashMap::new(),
        };

        let fields = vec![
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                ..Default::default()
            },
            FieldDefinition {
                name: "title".to_string(),
                field_type: FieldType::Text,
                ..Default::default()
            },
        ];

        let map = hook_ctx_to_string_map(&ctx, &fields);
        assert_eq!(map.get("seo__meta_title").unwrap(), "My Title");
        assert_eq!(map.get("seo__meta_description").unwrap(), "My Description");
        assert_eq!(map.get("title").unwrap(), "Hello");
        // The group key itself should not be present
        assert!(!map.contains_key("seo"));
    }

    #[test]
    fn test_hook_ctx_to_string_map_group_non_object_value() {
        // If a group field has a string value (e.g. from form data), fall through
        let mut data = HashMap::new();
        data.insert("seo".to_string(), json!("plain-string"));

        let ctx = HookContext {
            collection: "posts".to_string(),
            operation: "create".to_string(),
            data,
            locale: None,
            draft: None,
            context: HashMap::new(),
        };

        let fields = vec![FieldDefinition {
            name: "seo".to_string(),
            field_type: FieldType::Group,
            ..Default::default()
        }];

        let map = hook_ctx_to_string_map(&ctx, &fields);
        // Falls through to the string conversion
        assert_eq!(map.get("seo").unwrap(), "plain-string");
    }

    #[test]
    fn test_hook_ctx_to_string_map_group_with_numeric_subfields() {
        let mut data = HashMap::new();
        data.insert("metrics".to_string(), json!({
            "views": 100,
            "likes": 42
        }));

        let ctx = HookContext {
            collection: "posts".to_string(),
            operation: "create".to_string(),
            data,
            locale: None,
            draft: None,
            context: HashMap::new(),
        };

        let fields = vec![FieldDefinition {
            name: "metrics".to_string(),
            field_type: FieldType::Group,
            ..Default::default()
        }];

        let map = hook_ctx_to_string_map(&ctx, &fields);
        assert_eq!(map.get("metrics__views").unwrap(), "100");
        assert_eq!(map.get("metrics__likes").unwrap(), "42");
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
}
