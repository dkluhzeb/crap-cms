//! `HookRunner` methods for job execution and arbitrary Lua evaluation.

use anyhow::{Result, anyhow};
use mlua::Value;
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::{
    core::Document,
    db::DbConnection,
    hooks::{
        HookRunner,
        lifecycle::{InitPhase, execution::resolve_hook_function, types::TxContextGuard},
        lua_api,
    },
};

impl HookRunner {
    /// Execute a job handler function with CRUD access via `TxContext`.
    /// The handler receives a context table `{ data, job = { slug, attempt, max_attempts, queued_at } }`.
    /// Returns the handler's return value as JSON string (or None if nil).
    ///
    /// # Errors
    ///
    /// Returns an error if VM acquisition, handler resolution, the handler
    /// call itself, or return-value serialization fails.
    pub fn run_job_handler(
        &self,
        handler_ref: &str,
        slug: &str,
        data_json: &str,
        attempt: u32,
        max_attempts: u32,
        conn: &dyn DbConnection,
    ) -> Result<Option<String>> {
        let lua = self.pool.acquire()?;
        let _guard = TxContextGuard::set(&lua, conn, None, None, None);

        // Build context table
        let ctx = lua.create_table()?;

        // Parse data JSON into Lua table
        let data_value: JsonValue =
            serde_json::from_str(data_json).unwrap_or(JsonValue::Object(JsonMap::new()));
        let data_lua = lua_api::json_to_lua(&lua, &data_value)?;
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
        let return_val: Value = func.call(ctx)?;

        // Convert return value to JSON
        match return_val {
            Value::Nil => Ok(None),
            other => {
                let json_val = lua_api::lua_to_json(&other)?;

                Ok(Some(serde_json::to_string(&json_val)?))
            }
        }
    }

    /// Execute arbitrary Lua code within a transaction + user context.
    /// Used by integration tests for CRUD closure testing.
    ///
    /// # Errors
    ///
    /// Returns an error if VM acquisition or Lua evaluation fails.
    pub fn eval_lua_with_conn(
        &self,
        code: &str,
        conn: &dyn DbConnection,
        user: Option<&Document>,
    ) -> Result<String> {
        let lua = self.pool.acquire()?;
        let _guard = TxContextGuard::set(&lua, conn, user.cloned(), None, None);

        lua.load(code).eval::<String>().map_err(|e| anyhow!("{e}"))
    }

    /// Like [`eval_lua_with_conn`] but with [`InitPhase`] set on the VM,
    /// mirroring the state during `init.lua` and definition-file loading.
    /// Used by integration tests that exercise definition-file APIs
    /// (`crap.collections.define`, `crap.globals.define`,
    /// `crap.jobs.define`, `crap.richtext.register_node`) which are
    /// init-only at runtime.
    ///
    /// # Errors
    ///
    /// Returns an error if VM acquisition or Lua evaluation fails.
    pub fn eval_lua_init_with_conn(
        &self,
        code: &str,
        conn: &dyn DbConnection,
        user: Option<&Document>,
    ) -> Result<String> {
        let lua = self.pool.acquire()?;
        let _guard = TxContextGuard::set(&lua, conn, user.cloned(), None, None);

        lua.set_app_data(InitPhase);
        let r = lua.load(code).eval::<String>().map_err(|e| anyhow!("{e}"));
        lua.remove_app_data::<InitPhase>();
        r
    }
}
