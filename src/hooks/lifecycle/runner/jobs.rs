//! HookRunner methods for job execution and arbitrary Lua evaluation.

use anyhow::{Result, anyhow};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::{
    core::Document,
    hooks::{
        HookRunner, api,
        lifecycle::{execution::resolve_hook_function, types::TxContextGuard},
    },
};

impl HookRunner {
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
        conn: &dyn crate::db::DbConnection,
    ) -> Result<Option<String>> {
        let lua = self.pool.acquire()?;
        let _guard = TxContextGuard::set(&lua, conn, None, None);

        // Build context table
        let ctx = lua.create_table()?;

        // Parse data JSON into Lua table
        let data_value: JsonValue =
            serde_json::from_str(data_json).unwrap_or(JsonValue::Object(JsonMap::new()));
        let data_lua = api::json_to_lua(&lua, &data_value)?;
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
                let json_val = api::lua_to_json(&lua, &other)?;
                Ok(Some(serde_json::to_string(&json_val)?))
            }
        }
    }

    /// Execute arbitrary Lua code within a transaction + user context.
    /// The Lua code must return a string. Useful for testing CRUD closures.
    #[allow(dead_code)]
    pub fn eval_lua_with_conn(
        &self,
        code: &str,
        conn: &dyn crate::db::DbConnection,
        user: Option<&Document>,
    ) -> Result<String> {
        let lua = self.pool.acquire()?;
        let _guard = TxContextGuard::set(&lua, conn, user.cloned(), None);

        lua.load(code)
            .eval::<String>()
            .map_err(|e| anyhow!("{}", e))
    }
}
