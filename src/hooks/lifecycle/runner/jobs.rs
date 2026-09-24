//! `HookRunner` methods for job execution and arbitrary Lua evaluation.

use std::{cell::RefCell, rc::Rc};

use anyhow::{Result, anyhow};
use mlua::Value;
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::{
    core::{Document, JobDefinition, job::JobRun},
    db::{DbConnection, DbPool},
    hooks::{
        HookRunner, LuaCrudInfra,
        lifecycle::{
            ExecutionDeadline, ExecutionDeadlineGuard, InitPhase, JobHandlerContext, JobInfo,
            execution::resolve_hook_function, types::TxContextGuard,
        },
        lua_api,
    },
    service::{EventQueue, ServiceContext, flush_queue},
};

use super::vm_pool::DeadlineHookGuard;

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
    /// The handler runs under the job's `timeout` as a cooperative deadline
    /// (see `ExecutionDeadline`), with the VM hook armed for it: once it
    /// passes, the VM hook and every database / HTTP / email entry point
    /// raise a timeout error, and the operation in flight is rolled back
    /// rather than committed late. The
    /// handler therefore stops itself — the scheduler never has to abandon a
    /// run that is still executing.
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
        job: &JobDefinition,
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

        let result = self.run_job_handler_in_vm(job, job_run, pool, infra);

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
        job: &JobDefinition,
        job_run: &JobRun,
        pool: &DbPool,
        infra: Option<LuaCrudInfra>,
    ) -> Result<Option<String>> {
        let handler = &job.handler;
        let lua = self.pool.acquire()?;
        let _guard = TxContextGuard::set_pool(&lua, pool.clone(), None, None, infra);
        let _deadline = ExecutionDeadlineGuard::install(&lua, ExecutionDeadline::new(job.timeout));
        let _hook = DeadlineHookGuard::arm(&lua)
            .map_err(|e| anyhow!("failed to arm the Lua VM hook: {e}"))?;

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
        let ctx_value = lua_api::to_lua_value(&lua, &ctx)?;

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

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::{
        fs,
        path::Path,
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };

    use anyhow::Result;

    use crate::{
        config::CrapConfig,
        core::{CollectionDefinition, FieldDefinition, FieldType, JobDefinition, JobRun, Registry},
        db::{DbConnection, DbPool, migrate, pool},
        hooks::HookRunner,
    };

    /// Job handlers that never finish on their own.
    const RUNAWAY_JOBS: &str = r#"
local M = {}

function M.write_forever()
    while true do
        crap.collections.create("ticks", { label = "tick" })
    end
end

function M.compute_forever()
    local n = 0
    while true do n = n + 1 end
end

return M
"#;

    /// A migrated pool over a `ticks` collection and a runner whose config dir
    /// holds the runaway handlers.
    fn setup(dir: &Path) -> (DbPool, HookRunner) {
        fs::create_dir_all(dir.join("jobs")).expect("jobs dir");
        fs::write(dir.join("jobs/runaway.lua"), RUNAWAY_JOBS).expect("job file");

        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        // No instruction budget: the deadline alone has to stop the handlers.
        config.hooks.max_instructions = 0;
        let db_pool = pool::create_pool(dir, &config).expect("create pool");

        let mut ticks = CollectionDefinition::new("ticks");
        ticks.fields = vec![FieldDefinition::builder("label", FieldType::Text).build()];

        let shared = Registry::shared();
        shared.write().expect("registry").register_collection(ticks);
        let registry = Registry::snapshot(&shared);
        migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

        let runner = HookRunner::builder()
            .config_dir(dir)
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .expect("hook runner");

        (db_pool, runner)
    }

    fn run(runner: &HookRunner, db_pool: &DbPool, handler: &str) -> Result<Option<String>> {
        let job = JobDefinition::builder("runaway", handler)
            .timeout(1)
            .build();
        let job_run = JobRun::builder("runaway-run", "runaway")
            .data("{}")
            .attempt(1)
            .max_attempts(1)
            .build();

        runner.run_job_handler(&job, &job_run, db_pool, None)
    }

    fn tick_count(db_pool: &DbPool) -> i64 {
        let conn = db_pool.get().expect("read connection");

        conn.query_one("SELECT COUNT(*) FROM ticks", &[])
            .expect("count ticks")
            .and_then(|r| r.i64_at(0))
            .unwrap_or(0)
    }

    /// Regression: a job's `timeout` did not stop its handler. The scheduler's
    /// timer could only abandon the blocking task, which kept writing while
    /// the run was re-queued — so the retry ran next to it and both committed.
    /// The handler now stops itself at its next database call once the
    /// deadline passes, and nothing it does after that commits.
    #[test]
    fn a_job_writing_in_a_loop_stops_at_its_timeout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (db_pool, runner) = setup(tmp.path());
        let started = Instant::now();

        let err = run(&runner, &db_pool, "jobs.runaway.write_forever")
            .expect_err("the handler must stop at its timeout");

        assert!(
            format!("{err:#}").contains("exceeded its timeout"),
            "unexpected error: {err:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the handler ran long past its 1s timeout"
        );

        let written = tick_count(&db_pool);
        assert!(written > 0, "writes before the deadline stay committed");

        thread::sleep(Duration::from_millis(200));
        assert_eq!(
            tick_count(&db_pool),
            written,
            "nothing may be written once the handler has returned"
        );
    }

    /// A handler that never reaches a database call stops at its timeout too
    /// — through the VM hook, with no instruction budget needed.
    #[test]
    fn a_cpu_bound_job_stops_at_its_timeout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (db_pool, runner) = setup(tmp.path());
        let started = Instant::now();

        let err = run(&runner, &db_pool, "jobs.runaway.compute_forever")
            .expect_err("the handler must stop at its timeout");

        assert!(
            format!("{err:#}").contains("exceeded its timeout"),
            "unexpected error: {err:#}"
        );
        assert!(started.elapsed() < Duration::from_secs(30));
    }
}
