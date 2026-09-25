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
//! `crap.transaction(fn)` does not open a transaction of its own: the ops
//! keep sharing the outer tx, and the block runs as one savepoint-backed
//! step of it — an error inside the block rolls back the block's writes
//! only, even when the caller catches it with `pcall`.
//!
//! The transaction scope itself ([`run_scoped_tx`]) is shared with the
//! per-op transaction a bare pool-mode CRUD call opens (`with_lua_db`), so
//! `crap.tx.*`, event gating, file cleanup, and cache invalidation behave
//! identically at both commit points.

mod scope;

use anyhow::Result;
use mlua::{Error::RuntimeError, Function, Lua, Result as LuaResult, Value};

use crate::{
    db::{DbConnection, DbPool, InPlaceTransaction},
    hooks::{
        lifecycle::{PoolContext, TxContext, check_execution_deadline},
        lua_api::crud::{TxSlot, ensure_writable, get_tx_conn, open_lazy_tx_to_write, run_step},
    },
};

pub(crate) use scope::TxScope;

/// The write pool of the installed [`PoolContext`].
///
/// # Errors
///
/// Returns a Lua runtime error naming `label` when no pool context is
/// installed.
fn scoped_pool(lua: &Lua, label: &str) -> LuaResult<DbPool> {
    let ctx = lua.app_data_ref::<PoolContext>().ok_or_else(|| {
        RuntimeError(format!(
            "{label} requires a job or pool context — call it from inside a Lua job \
             handler, a custom route handler, or an effect, not from init.lua / \
             collection definitions / top-level scripts"
        ))
    })?;

    Ok(ctx.pool.clone())
}

/// Run `work` inside a fresh IMMEDIATE transaction with the FULL transaction
/// scope every commit point shares ([`TxScope`]):
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

    let conn = scoped_pool(lua, label)?
        .write()
        .map_err(|e| RuntimeError(format!("{label}: pool.write: {e}")))?;
    let tx = InPlaceTransaction::begin_owned(conn)
        .map_err(|e| RuntimeError(format!("{label}: begin: {e}")))?;

    let scope = TxScope::open(lua, label);

    // SAFETY: TxContext stores a fat pointer to the transaction's connection.
    // `tx` lives on this function's stack until `settle` consumes it, and
    // `TxSlot` removes the pointer when the inner scope ends — including if
    // the closure unwinds — so it is never dereferenced after the tx is gone.
    lua.set_app_data(TxContext::new(tx.conn()));
    let call_result = {
        let _slot = TxSlot(lua);

        work(tx.conn())
    };

    // The last point at which a job past its deadline can still be stopped
    // without committing late.
    let call_result = call_result.and_then(|value| check_execution_deadline(lua).map(|()| value));
    let commit = call_result.is_ok();

    scope.settle(tx, call_result, commit, |e| {
        RuntimeError(format!("{label}: commit: {e:#}"))
    })
}

/// Wrap a Lua closure in a single IMMEDIATE transaction.
///
/// Inside an enclosing transaction (a hook, an auth hook's transaction, an
/// outer `crap.transaction`) the block is one atomic step of it instead: it
/// runs in a savepoint, so a block that errors — even one whose error the
/// caller catches with `pcall` — leaves none of its writes behind while the
/// enclosing transaction goes on.
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
    open_lazy_tx_to_write(lua)?;

    if lua.app_data_ref::<TxContext>().is_none() {
        return run_scoped_tx(lua, "crap.transaction", |_| fn_arg.call::<Value>(()));
    }

    let conn = get_tx_conn(lua)?;

    run_step(lua, conn, |_| fn_arg.call::<Value>(()))
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
--- already runs in the parent's write transaction) the block joins that
--- transaction as one atomic step: an error inside it rolls back the
--- block's writes only.
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
