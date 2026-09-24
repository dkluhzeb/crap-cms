//! Register `crap.transaction(fn)` — explicit multi-step atomicity
//! for Lua job handlers.
//!
//! Inside a job (pool-mode), each Lua CRUD call opens its own
//! short-lived IMMEDIATE transaction (via the `auto_tx` attribute on
//! every CRUD `#[lua_fn]`). That model removes the
//! `SQLITE_BUSY_SNAPSHOT` hazard but loses cross-op atomicity: a
//! `find` followed by an `update` are two distinct transactions, and a
//! crash between them leaves the read's logical preconditions
//! unverified.
//!
//! `crap.transaction(function() … end)` opts back into a single
//! shared transaction for the duration of the closure. Implementation:
//!
//! - Open `BEGIN IMMEDIATE` on a fresh pool connection.
//! - Install `TxContext` (conn-mode) in Lua `app_data` for the
//!   duration of the closure call — `with_lua_db` sees `TxContext`
//!   first and reuses the shared tx for nested CRUD ops.
//! - On `Ok` return: remove `TxContext`, `COMMIT`, return the connection to
//!   the write pool, then run any `crap.tx.on_commit` effects registered
//!   inside the closure.
//! - On `Err` return: remove `TxContext`, drop the tx → automatic
//!   rollback, return the connection, then run any `crap.tx.on_rollback`
//!   compensations.
//!
//! The connection goes back BEFORE the effects run on both paths: an effect's
//! own CRUD opens a scoped transaction on a fresh write-pool checkout, which
//! must not have to wait for the slot this scope was holding.
//!
//! Inside a hook (already conn-mode with the parent's write tx),
//! `crap.transaction(fn)` is a pass-through: call `fn` directly so the
//! ops continue to share the outer tx. Nested explicit transactions
//! aren't supported (no `SAVEPOINT` mechanism in this alpha — defer
//! until a real use case surfaces).
//!
//! The transaction scope itself ([`run_scoped_tx`]) is shared with the
//! per-op transaction a bare pool-mode CRUD call opens (`with_lua_db`), so
//! `crap.tx.*`, event gating, file cleanup, and cache invalidation behave
//! identically at both commit points.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use anyhow::Result;
use mlua::{Error::RuntimeError, Function, Lua, Result as LuaResult, Value};
use tracing::warn;

use crate::{
    core::{SharedCache, upload::delete_storage_keys},
    db::DbConnection,
    hooks::{
        lifecycle::{
            FileCleanupQueue, LuaCrudInfra, LuaVmInfra, PoolContext, TxContext,
            check_execution_deadline, run_effects_on_vm,
        },
        lua_api::crud::{TxSlot, ensure_writable},
    },
    service::{DeferredEffect, DeferredQueue, EffectOutcome, EventQueue, VerificationQueue},
};

/// Post-commit populate-cache invalidation for a `crap.transaction` scope:
/// hand the dirty flag up to an enclosing scope if there is one, else clear the
/// cache now (the write is durable).
fn flush_cache_dirty(
    dirty: &Cell<bool>,
    outer: Option<&Rc<Cell<bool>>>,
    cache: Option<&SharedCache>,
) {
    if !dirty.get() {
        return;
    }

    if let Some(outer) = outer {
        outer.set(true);
    } else if let Some(cache) = cache
        && let Err(e) = cache.clear()
    {
        warn!("crap.transaction: cache clear failed: {e:#}");
    }
}

/// The queues one scoped transaction owns, beside the enclosing scope's
/// queues they hand up to on commit (`None` = no enclosing scope).
///
/// FRESH event/verification queues are scoped to the transaction (frozen
/// contract: a rolled-back write never emits an event). The ambient job-level
/// queues flush unconditionally after the handler — routing inner-CRUD events
/// there directly would publish them even when this transaction rolls back.
/// On commit the events are handed up; on rollback they are dropped, exactly
/// like `run_pool_write`.
struct ScopedQueues {
    tx_events: EventQueue,
    outer_events: Option<EventQueue>,
    tx_verifications: VerificationQueue,
    outer_verifications: Option<VerificationQueue>,
    tx_files: FileCleanupQueue,
    outer_files: Option<FileCleanupQueue>,
    tx_cache_dirty: Rc<Cell<bool>>,
    outer_cache_dirty: Option<Rc<Cell<bool>>>,
    /// The cache handle, captured before `infra` moves into `app_data`, so the
    /// post-commit flush can clear it when there is no enclosing scope.
    cache: Option<SharedCache>,
}

impl ScopedQueues {
    /// Swap fresh per-transaction queues into `infra`, keeping the enclosing
    /// scope's queues to hand up to on commit.
    fn install(infra: &mut LuaCrudInfra) -> Self {
        let tx_events: EventQueue = Rc::new(RefCell::new(Vec::new()));
        let tx_verifications: VerificationQueue = Rc::new(RefCell::new(Vec::new()));
        let tx_files: FileCleanupQueue = Rc::new(RefCell::new(Vec::new()));
        let tx_cache_dirty: Rc<Cell<bool>> = Rc::new(Cell::new(false));

        Self {
            outer_events: infra.event_queue.replace(tx_events.clone()),
            outer_verifications: infra.verification_queue.replace(tx_verifications.clone()),
            outer_files: infra.file_cleanup.replace(tx_files.clone()),
            outer_cache_dirty: infra.cache_dirty.replace(tx_cache_dirty.clone()),
            cache: infra.cache.clone(),
            tx_events,
            tx_verifications,
            tx_files,
            tx_cache_dirty,
        }
    }

    /// The transaction committed: hand its events, verifications, upload
    /// files and cache-dirty flag up to the enclosing scope — or, with none,
    /// publish, delete and clear now. Needs no database connection.
    fn settle_after_commit(&self, lua: &Lua, label: &str) {
        self.hand_up_events(label);
        self.hand_up_verifications(label);
        self.settle_files(lua);

        flush_cache_dirty(
            &self.tx_cache_dirty,
            self.outer_cache_dirty.as_ref(),
            self.cache.as_ref(),
        );
    }

    /// Hand the transaction's events up to the ambient (job-level) queue,
    /// which flushes post-handler.
    fn hand_up_events(&self, label: &str) {
        if let Some(outer) = &self.outer_events {
            outer
                .borrow_mut()
                .extend(self.tx_events.borrow_mut().drain(..));

            return;
        }

        if !self.tx_events.borrow().is_empty() {
            warn!(
                "{label}: {} event(s) from a committed transaction had no ambient queue \
                 to flush into and were dropped",
                self.tx_events.borrow().len()
            );
        }
    }

    /// Hand the transaction's pending verifications up to the enclosing
    /// service write, which sends them after its own commit.
    ///
    /// A verification only lands in this queue when the write had no email
    /// context to issue it in-transaction; every surface that owns its
    /// transaction (jobs, routes, migrations, `on_init`) carries one, so with
    /// no enclosing scope the queue stays empty. Should anything arrive here
    /// anyway it is reported, never dropped silently — an account without its
    /// verification can neither verify nor sign up again.
    fn hand_up_verifications(&self, label: &str) {
        if let Some(outer) = &self.outer_verifications {
            outer
                .borrow_mut()
                .extend(self.tx_verifications.borrow_mut().drain(..));

            return;
        }

        for pending in self.tx_verifications.borrow_mut().drain(..) {
            warn!(
                "{label}: account {} in '{}' requires email verification, but this \
                 transaction has no email context to issue it through — no \
                 verification was sent",
                pending.doc_id, pending.slug
            );
        }
    }

    /// Upload files from hard deletes inside this transaction: hand up to an
    /// enclosing scope, or — with none — delete NOW (we are post-commit) via
    /// the VM's storage handle.
    fn settle_files(&self, lua: &Lua) {
        if let Some(outer) = &self.outer_files {
            outer
                .borrow_mut()
                .extend(self.tx_files.borrow_mut().drain(..));

            return;
        }

        let Some(storage) = lua
            .app_data_ref::<LuaVmInfra>()
            .and_then(|i| i.storage.clone())
        else {
            return;
        };

        let keys: Vec<String> = self.tx_files.borrow_mut().drain(..).collect();
        delete_storage_keys(&*storage, &keys);
    }
}

/// Run `work` inside a fresh IMMEDIATE transaction with the FULL transaction
/// scope every commit point shares:
///
/// - a per-transaction `crap.tx.on_commit` / `on_rollback` queue, so a hook
///   fired by CRUD inside `work` can register effects (they run after the
///   outcome, in pool-mode);
/// - fresh event / verification queues handed up to the ambient (job or
///   route) queues only on commit — a rolled-back write never emits;
/// - a per-transaction upload file-cleanup queue drained after commit, and a
///   populate-cache dirty flag cleared after commit.
///
/// This is the ONE implementation behind both pool-mode commit points:
/// `crap.transaction(fn)` and the per-op transaction `with_lua_db` opens for a
/// bare CRUD call in a job / route / effect. A hook therefore behaves the
/// same whether its write came through the service envelope, an explicit
/// transaction, or a bare call.
///
/// `label` prefixes the pool/begin/commit error text.
///
/// A job handler's deadline (see `ExecutionDeadline`) is checked on entry and
/// again right before `COMMIT`: an operation still in flight when the
/// deadline passes — one blocked on the database lock, say — is rolled back
/// instead of committing after the job was already reported as timed out.
///
/// # Errors
///
/// Returns a Lua runtime error when no pool context is installed, when the
/// running job's deadline has passed, when the transaction cannot be opened
/// or committed, or when `work` errors (the transaction is rolled back and
/// compensations run first).
pub(crate) fn run_scoped_tx<R>(
    lua: &Lua,
    label: &str,
    work: impl FnOnce(&dyn DbConnection) -> LuaResult<R>,
) -> LuaResult<R> {
    check_execution_deadline(lua)?;

    let pool = lua
        .app_data_ref::<PoolContext>()
        .ok_or_else(|| {
            RuntimeError(format!(
                "{label} requires a job or pool context — call it from inside a Lua job \
                 handler, a custom route handler, or an effect, not from init.lua / \
                 collection definitions / top-level scripts"
            ))
        })?
        .pool
        .clone();

    let mut conn = pool
        .write()
        .map_err(|e| RuntimeError(format!("{label}: pool.write: {e}")))?;
    let tx = conn
        .transaction_immediate()
        .map_err(|e| RuntimeError(format!("{label}: begin: {e}")))?;

    // Per-transaction queue for `crap.tx.on_commit` / `on_rollback`
    // registrations inside the closure. Installed by swapping a modified
    // `LuaCrudInfra` into app_data (snapshot/restore — the same stack
    // discipline as `TxContextGuard`).
    let dq: DeferredQueue = Rc::new(RefCell::new(Vec::new()));
    let prev_infra = lua.app_data_ref::<LuaCrudInfra>().map(|r| (*r).clone());

    let mut infra = prev_infra.clone().unwrap_or_default();
    infra.deferred = Some(dq.clone());

    let queues = ScopedQueues::install(&mut infra);
    lua.set_app_data(infra);

    // SAFETY: TxContext stores a fat pointer to `&tx`. `tx` lives on this
    // function's stack until just below, and `TxSlot` removes the pointer
    // when the inner scope ends — including if the closure unwinds — so it
    // is never dereferenced after the tx is gone.
    lua.set_app_data(TxContext::new(&tx));
    let call_result = {
        let _slot = TxSlot(lua);
        // RAII like `TxSlot` above: a Rust panic unwinding out of a
        // `crap.*` callback must not return this VM to the pool with the
        // transaction-scoped `LuaCrudInfra` (deferred queue, caches) still
        // installed — a later request would then see a live deferred
        // queue outside any transaction and `crap.tx.on_commit` would
        // silently no-op into it.
        let _infra_slot = InfraRestore {
            lua,
            prev: prev_infra,
        };

        work(&tx)
    };

    // The last point at which a job past its deadline can still be stopped
    // without committing late.
    let call_result = call_result.and_then(|value| check_execution_deadline(lua).map(|()| value));

    let effects: Vec<DeferredEffect> = dq.borrow_mut().drain(..).collect();

    let value = match call_result {
        Ok(value) => value,
        Err(e) => {
            // Roll back AND return the write-pool slot BEFORE compensations
            // run: their pool-mode CRUD opens a scoped transaction of its
            // own, which checks a second write connection out. With this
            // slot still held, a write pool of one would make that checkout
            // wait for the pool timeout and fail; a larger pool would pin
            // two slots per job.
            drop(tx);
            drop(conn);
            run_effects_on_vm(lua, &effects, EffectOutcome::Rollback);

            return Err(e);
        }
    };

    let commit_result = tx.commit();
    // Same release-before-effects rule on the commit side: nothing below
    // touches the database through this connection, and the effects need
    // the slot it holds.
    drop(conn);

    if let Err(e) = commit_result {
        // A failed commit is a rollback outcome — the queued
        // events/verifications die with the transaction.
        run_effects_on_vm(lua, &effects, EffectOutcome::Rollback);

        return Err(RuntimeError(format!("{label}: commit: {e}")));
    }

    queues.settle_after_commit(lua, label);

    // Effects run in THIS VM: `PoolContext` is live again (job
    // context), so effect CRUD is pool-mode, and events queue into
    // the job's own event queue (flushed post-handler).
    run_effects_on_vm(lua, &effects, EffectOutcome::Commit);

    Ok(value)
}

/// Wrap a Lua closure in a single IMMEDIATE transaction.
///
/// Errors out if called outside a job context (no `PoolContext` and no
/// `TxContext` — e.g., from `init.lua` or a top-level script).
#[allow(clippy::needless_pass_by_value)]
fn lua_transaction(lua: &Lua, fn_arg: Function) -> LuaResult<Value> {
    // Checked first, for the same reason `with_lua_db` does: a read-only
    // render context installs a `TxContext` while a read is in flight, and
    // the pass-through below would otherwise hand that read connection to a
    // block whose whole purpose is to write.
    ensure_writable(lua)?;

    // Pass-through: already inside a shared tx (hook context, or a
    // surrounding `crap.transaction(fn)`). Call `fn` directly.
    if lua.app_data_ref::<TxContext>().is_some() {
        return fn_arg.call::<Value>(());
    }

    run_scoped_tx(lua, "crap.transaction", |_| fn_arg.call::<Value>(()))
}

/// RAII restore for the transaction-scoped [`LuaCrudInfra`] swap. On
/// drop — return OR unwind — the previous infra (or absence) is put
/// back, mirroring the stack discipline `TxSlot` gives `TxContext`.
struct InfraRestore<'a> {
    lua: &'a Lua,
    /// The infra to put back on drop; `None` = there was none before,
    /// so the slot is removed. (`Drop` runs exactly once, so a plain
    /// `Option` suffices — `take()` just moves the value out.)
    prev: Option<LuaCrudInfra>,
}

impl Drop for InfraRestore<'_> {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(p) => {
                self.lua.set_app_data(p);
            }
            None => {
                self.lua.remove_app_data::<LuaCrudInfra>();
            }
        }
    }
}

/// Register `crap.transaction(fn)` on the given Lua VM.
///
/// # Errors
///
/// Returns an error if function creation or setting on the `crap` table
/// fails.
pub(crate) fn register_transaction(lua: &Lua) -> Result<()> {
    let crap: mlua::Table = lua.globals().get("crap")?;
    let f = lua.create_function(lua_transaction)?;
    crap.set("transaction", f)?;
    Ok(())
}

/// Render `crap.transaction(fn)` into the generated `types/crap.lua`.
/// Hand-written because the function takes a Lua closure (`fn:
/// fun(): T`) and returns its result, which neither `#[lua_fn]`'s
/// auto-typing nor the manual `LuaFnSpec` machinery can express
/// today (no generic return).
pub(crate) fn render_crap_transaction_lua(out: &mut String) {
    out.push_str(
        "\
-- ── crap.transaction — explicit multi-step atomicity ───────────────

--- Wrap `fn` in a single IMMEDIATE transaction. Use inside job
--- handlers when multiple CRUD operations need to be atomic — by
--- default each Lua CRUD call in a job opens its own short-lived
--- transaction (pool-mode), so a `find` followed by an `update` are
--- two separate writes. Wrap them in `crap.transaction(function()
--- ... end)` to make the block atomic.
---
--- Returns whatever `fn` returns. Errors raised inside `fn` roll back
--- the transaction and propagate as Lua errors. Inside a hook (which
--- already runs in the parent's write transaction) this is a
--- pass-through.
---
--- Only valid from a job handler — calling from init.lua / collection
--- definitions / top-level scripts raises a runtime error.
---
--- @generic T
--- @param fn fun(): T
--- @return T
function crap.transaction(fn) end

",
    );
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::{path::PathBuf, sync::Arc};

    use mlua::{Error::RuntimeError, Lua};

    use crate::{
        config::CrapConfig,
        core::{
            CollectionDefinition, FieldDefinition, FieldType, HookRef, JobDefinition, JobRun,
            Registry, collection::Hooks,
        },
        db::{DbConnection, DbPool, migrate, pool, query},
        hooks::{
            HookRunner,
            lifecycle::{ExecutionDeadline, PoolContext, PoolMode},
        },
    };

    use super::run_scoped_tx;

    /// The `crap.tx` fixture tree: `jobs.tx_job.run_commit` wraps a create in
    /// `crap.transaction` and registers `hooks.effects.log_commit`, which
    /// writes a `tx_log` row through pool-mode CRUD.
    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tx_outcome")
    }

    fn tx_articles() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("tx_articles");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("boom", FieldType::Text).build(),
        ];
        def.hooks = Hooks {
            before_change: vec![HookRef::new("hooks.effects.register")],
            ..Default::default()
        };

        def
    }

    fn tx_log() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("tx_log");
        def.fields = vec![FieldDefinition::builder("message", FieldType::Text).build()];

        def
    }

    /// A migrated pool over the fixture collections with the given database
    /// config, and a runner over the fixture hooks.
    fn setup(config: &CrapConfig, tmp: &tempfile::TempDir) -> (DbPool, Arc<Registry>, HookRunner) {
        let db_pool = pool::create_pool(tmp.path(), config).expect("create pool");

        let shared = Registry::shared();
        {
            let mut reg = shared.write().expect("registry");
            reg.register_collection(tx_articles());
            reg.register_collection(tx_log());
        }
        let registry = Registry::snapshot(&shared);
        migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

        let runner = HookRunner::builder()
            .config_dir(&fixture_dir())
            .registry(Arc::clone(&registry))
            .config(config)
            .build()
            .expect("hook runner");

        (db_pool, registry, runner)
    }

    fn log_messages(db_pool: &DbPool, registry: &Registry) -> Vec<String> {
        let def = registry.get_collection("tx_log").expect("tx_log").clone();
        let conn = db_pool.get().expect("connection");
        let docs = query::find(&conn, "tx_log", &def, &query::FindQuery::default(), None)
            .expect("find tx_log");

        docs.iter()
            .filter_map(|d| d.fields.get("message").and_then(|v| v.as_str()))
            .map(String::from)
            .collect()
    }

    /// A `crap.tx.on_commit` effect writes through a scoped transaction of its
    /// own, which checks out a second write connection. With the pool's ONE
    /// write connection still held by the just-committed scope, that checkout
    /// waited out the pool timeout and the effect failed — its write never
    /// happened, and the job reported success regardless.
    #[test]
    fn a_commit_effect_can_write_with_a_write_pool_of_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        config.database.write_pool_max_size = 1;
        // Long enough that building the pool never times out when the whole
        // suite runs in parallel; the single write slot is what this pins.
        config.database.connection_timeout = 5;

        let (db_pool, registry, runner) = setup(&config, &tmp);

        let run = JobRun::builder("tx-test-run", "tx_test")
            .data("{}")
            .attempt(1)
            .max_attempts(1)
            .build();
        runner
            .run_job_handler(
                &JobDefinition::builder("tx_test", "jobs.tx_job.run_commit").build(),
                &run,
                &db_pool,
                None,
            )
            .expect("run_job_handler");

        let mut messages = log_messages(&db_pool, &registry);
        messages.sort();

        assert_eq!(
            messages,
            vec!["commit:in-tx:commit", "commit:job:commit"],
            "both on_commit effects must have written their row"
        );
    }

    /// The rollback side releases the slot the same way: the compensation's
    /// own write must go through with one write connection.
    #[test]
    fn a_rollback_effect_can_write_with_a_write_pool_of_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        config.database.write_pool_max_size = 1;
        // Long enough that building the pool never times out when the whole
        // suite runs in parallel; the single write slot is what this pins.
        config.database.connection_timeout = 5;

        let (db_pool, registry, runner) = setup(&config, &tmp);

        let run = JobRun::builder("tx-test-run", "tx_test")
            .data("{}")
            .attempt(1)
            .max_attempts(1)
            .build();
        runner
            .run_job_handler(
                &JobDefinition::builder("tx_test", "jobs.tx_job.run_rollback").build(),
                &run,
                &db_pool,
                None,
            )
            .expect("run_job_handler");

        let mut messages = log_messages(&db_pool, &registry);
        messages.sort();

        assert_eq!(
            messages,
            vec!["rollback:doomed:rollback", "rollback:job:rollback"],
            "both on_rollback compensations must have written their row"
        );
    }

    /// Regression: an operation still in flight when a job's deadline passed
    /// (one waiting on the database lock, say) committed after the scheduler
    /// had already recorded the timeout and re-queued the run. The scope now
    /// re-checks the deadline right before `COMMIT` and rolls back instead.
    #[test]
    fn an_operation_in_flight_when_the_job_deadline_passes_is_rolled_back() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

        db_pool
            .write()
            .expect("write connection")
            .execute("CREATE TABLE deadline_probe (id TEXT)", &[])
            .expect("probe table");

        let lua = Lua::new();
        lua.set_app_data(PoolContext {
            pool: db_pool.clone(),
            mode: PoolMode::Write,
        });

        let result = run_scoped_tx(&lua, "test", |conn| {
            conn.execute("INSERT INTO deadline_probe (id) VALUES ('late')", &[])
                .map_err(|e| RuntimeError(e.to_string()))?;

            // The deadline passes while the operation is still running.
            lua.set_app_data(ExecutionDeadline::new(0));

            Ok(())
        });
        lua.remove_app_data::<ExecutionDeadline>();

        let Err(err) = result else {
            panic!("an operation that outlived the job deadline must not commit");
        };
        assert!(
            err.to_string().contains("exceeded its timeout"),
            "got: {err}"
        );

        let conn = db_pool.get().expect("read connection");
        let row = conn
            .query_one("SELECT COUNT(*) FROM deadline_probe", &[])
            .expect("count probe rows");

        assert_eq!(
            row.and_then(|r| r.i64_at(0)),
            Some(0),
            "the late write must have been rolled back"
        );
    }
}
