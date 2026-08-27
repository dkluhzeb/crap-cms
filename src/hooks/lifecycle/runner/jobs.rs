//! `HookRunner` methods for job execution and arbitrary Lua evaluation.

use std::{cell::RefCell, rc::Rc};

use anyhow::{Result, anyhow};
use mlua::{LuaSerdeExt as _, Value};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::{
    core::{Document, HookRef, job::JobRun},
    db::{DbConnection, DbPool},
    hooks::{
        HookRunner, LuaCrudInfra,
        lifecycle::{
            InitPhase, JobHandlerContext, JobInfo, execution::resolve_hook_function,
            types::TxContextGuard,
        },
        lua_api,
    },
    service::{EventQueue, ServiceContext, flush_queue},
};

impl HookRunner {
    /// Execute a job handler function in **pool-mode**: no outer
    /// transaction is opened. Each Lua CRUD call inside the handler
    /// opens its own short-lived IMMEDIATE transaction via
    /// `with_lua_db` (wired by the `#[lua_fn(auto_tx)]` attribute).
    ///
    /// The handler receives a context table `{ data, job }`, where `job` is
    /// `{ id, slug, queue, attempt, max_attempts, priority, unique_key,
    /// scheduled_by, queued_at }`. Returns the handler's return value as a JSON
    /// string (or `None` for nil).
    ///
    /// For multi-step atomicity, user code wraps a block in
    /// `crap.transaction(function() ... end)` which temporarily swaps
    /// the pool context for a single shared tx context.
    ///
    /// `infra` carries the event transport and populate cache into the
    /// handler's Lua CRUD calls — without it, job-mode writes publish no
    /// live-update events (even with `events = true`) and never invalidate
    /// the populate cache. The scheduler passes it from its `AppInfra`.
    ///
    /// Events are queued during the handler and flushed after it returns:
    /// publishing needs the runner itself (`before_broadcast` hooks, live
    /// settings run in a VM), which is not reachable from inside the handler's
    /// VM — and post-handler is also post-commit for every per-op transaction
    /// the handler ran. The flush happens even when the handler errored, since
    /// earlier ops committed their own transactions.
    ///
    /// # Errors
    ///
    /// Returns an error if VM acquisition, handler resolution, the handler
    /// call itself, or return-value serialization fails.
    pub fn run_job_handler(
        &self,
        handler: &HookRef,
        job_run: &JobRun,
        pool: &DbPool,
        infra: Option<LuaCrudInfra>,
    ) -> Result<Option<String>> {
        let event_queue: Option<EventQueue> =
            infra.as_ref().map(|_| Rc::new(RefCell::new(Vec::new())));
        let event_transport = infra.as_ref().and_then(|i| i.event_transport.clone());
        let infra = infra.map(|mut i| {
            i.event_queue.clone_from(&event_queue);
            i
        });

        let result = self.run_job_handler_in_vm(handler, job_run, pool, infra);

        if let Some(queue) = event_queue {
            let flush_ctx = ServiceContext::slug_only("")
                .runner(self)
                .event_transport(event_transport)
                .build();
            flush_queue(&flush_ctx, &queue);
        }

        result
    }

    /// The VM-holding body of [`Self::run_job_handler`] — split out so the VM
    /// lease is released before the post-handler event flush (whose
    /// `before_broadcast` hooks acquire their own VM).
    fn run_job_handler_in_vm(
        &self,
        handler: &HookRef,
        job_run: &JobRun,
        pool: &DbPool,
        infra: Option<LuaCrudInfra>,
    ) -> Result<Option<String>> {
        let lua = self.pool.acquire()?;
        let _guard = TxContextGuard::set_pool(&lua, pool.clone(), None, None, infra);

        // Build context from a typed Rust struct so the Lua shape is
        // the single source of truth (see
        // `hooks::lifecycle::JobHandlerContext`).
        let data_value: JsonValue =
            serde_json::from_str(&job_run.data).unwrap_or(JsonValue::Object(JsonMap::new()));
        let ctx = JobHandlerContext {
            data: &data_value,
            job: JobInfo {
                id: &job_run.id,
                slug: &job_run.slug,
                queue: &job_run.queue,
                attempt: job_run.attempt,
                max_attempts: job_run.max_attempts,
                priority: job_run.priority,
                unique_key: job_run.unique_key.as_deref(),
                scheduled_by: job_run.scheduled_by.as_deref(),
                queued_at: job_run.created_at.as_deref(),
            },
            options: handler.options(),
        };
        let ctx_value = lua.to_value(&ctx)?;

        // Resolve the handler function (e.g., "jobs.cleanup.run")
        let func = resolve_hook_function(&lua, handler.reference())?;

        // Call handler(ctx)
        let return_val: Value = func.call(ctx_value)?;

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
