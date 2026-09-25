//! Helpers for retrieving the active DB connection from Lua `app_data`.
//! Used by every Lua CRUD closure. Two dispatch modes:
//!
//! - **Conn-mode** (hooks, `crap.transaction(fn)`): a single `TxContext`
//!   is set in `app_data` for the duration of the outer call; every CRUD
//!   op uses that shared transaction.
//! - **Pool-mode** (job handlers): a `PoolContext` is set instead, and
//!   each CRUD op opens its own short-lived `IMMEDIATE` transaction via
//!   the pool — with the full transaction scope `crap.transaction(fn)`
//!   gets (`run_scoped_tx`). Avoids the `SQLITE_BUSY_SNAPSHOT` hazard that
//!   fires when a long-running handler's read snapshot collides with
//!   concurrent writers.
//!
//! Callers should use [`with_lua_db`] which handles both modes uniformly;
//! [`get_tx_conn`] is the conn-mode-only path retained for hook-internal
//! code that knows the mode.

use std::{cell::RefCell, rc::Rc};

use mlua::{Error::RuntimeError, Lua, Result as LuaResult};

use crate::{
    db::{DbConnection, with_savepoint},
    hooks::{
        lifecycle::{
            AfterReadScope, LazyTxContext, LuaCrudInfra, PoolContext, PoolMode, ReadOnlyScope,
            TxContext, check_execution_deadline,
        },
        lua_api::transaction::run_scoped_tx,
    },
    service::{DeferredEffect, EffectOutcome},
};

/// Open the hook's pending lazy transaction (see
/// [`LazyTx`](crate::hooks::lifecycle::LazyTx)) at its first CRUD call and
/// install its connection as the `TxContext` every later call shares. A
/// no-op when a `TxContext` is already installed or no lazy transaction is
/// pending.
///
/// # Errors
///
/// Returns a Lua runtime error when the transaction cannot be opened (no
/// write connection, `BEGIN` failed).
pub(crate) fn open_lazy_tx(lua: &Lua) -> LuaResult<()> {
    if lua.app_data_ref::<TxContext>().is_some() {
        return Ok(());
    }

    install_lazy_tx(lua)
}

/// [`open_lazy_tx`] for a write: a write inside a read that runs on the
/// lazy transaction's reader ([`LazyReadScope`]) opens the transaction too,
/// instead of writing on the reader.
///
/// # Errors
///
/// As [`open_lazy_tx`].
pub(crate) fn open_lazy_tx_to_write(lua: &Lua) -> LuaResult<()> {
    let on_reader = lua.app_data_ref::<LazyReadScope>().is_some();

    if lua.app_data_ref::<TxContext>().is_some() && !on_reader {
        return Ok(());
    }

    install_lazy_tx(lua)
}

fn install_lazy_tx(lua: &Lua) -> LuaResult<()> {
    let Some(lazy) = lua.app_data_ref::<LazyTxContext>().map(|c| *c) else {
        return Ok(());
    };

    // SAFETY: the `LazyTxGuard` that installed `lazy` borrows the `LazyTx`
    // for as long as the context is installed, and on drop removes both it
    // and the `TxContext` set below — before the transaction can drop.
    let tx = unsafe { lazy.tx() };
    let conn = tx.conn().map_err(|e| RuntimeError(format!("{e:#}")))?;

    lua.set_app_data(TxContext::new(conn));

    Ok(())
}

/// Marks a read running on a pending lazy transaction's reader (see
/// [`open_lazy_tx_to_write`]).
struct LazyReadScope;

/// Removes the [`LazyReadScope`] marker — on the unwind path too.
struct LazyReadSlot<'a>(&'a Lua);

impl Drop for LazyReadSlot<'_> {
    fn drop(&mut self) {
        self.0.remove_app_data::<LazyReadScope>();
    }
}

/// The reader of the pending lazy transaction, while no write opened it and
/// no other connection context is installed.
fn lazy_reader(lua: &Lua) -> Option<&dyn DbConnection> {
    if lua.app_data_ref::<TxContext>().is_some() {
        return None;
    }

    let lazy = lua.app_data_ref::<LazyTxContext>().map(|c| *c)?;

    // SAFETY: as in `install_lazy_tx`.
    unsafe { lazy.tx() }.reader()
}

/// Run a read on the pending lazy transaction's `reader`, in autocommit.
/// The reader is installed as the `TxContext` for the call only, so reads
/// nested in it share it — while a write nested in it opens the transaction
/// ([`open_lazy_tx_to_write`]).
fn read_on_lazy_reader<R>(
    lua: &Lua,
    reader: &dyn DbConnection,
    work: impl FnOnce(&dyn DbConnection) -> LuaResult<R>,
) -> LuaResult<R> {
    // SAFETY: `reader` outlives this call (the lazy transaction borrows it
    // for the hook's run), and `TxSlot` removes the pointer when the call
    // ends — including on unwind.
    lua.set_app_data(TxContext::new(reader));
    lua.set_app_data(LazyReadScope);

    let _slot = TxSlot(lua);
    let _read = LazyReadSlot(lua);

    work(reader)
}

/// Get the active transaction connection from Lua `app_data`.
/// Returns an error if no `TxContext` is set (i.e. called outside hook
/// context or `crap.transaction(fn)`).
///
/// The returned reference is valid for the duration of the current hook
/// call. `TxContextGuard` (set by the runner) keeps the underlying
/// connection alive until the hook returns.
///
/// Prefer [`with_lua_db`] over this — it transparently handles pool-mode
/// (jobs) as well. Direct callers of `get_tx_conn` are restricted to
/// conn-mode contexts.
pub(crate) fn get_tx_conn(lua: &Lua) -> LuaResult<&dyn DbConnection> {
    open_lazy_tx(lua)?;

    let ctx = lua.app_data_ref::<TxContext>().ok_or_else(|| {
        RuntimeError(
            "crap.collections CRUD functions need a database context — call \
             them inside a lifecycle hook (before_change, before_delete, \
             etc.), a job handler, a custom route handler, or wrap the call \
             in crap.transaction(fn)"
                .into(),
        )
    })?;
    let ptr = ctx.as_ptr();
    // SAFETY: `TxContextGuard` (constructed in `run_hooks_with_conn` and
    // friends) holds the connection borrow for the full duration of this hook
    // call. The guard removes the `TxContext` from app_data on drop, which
    // strictly outlives any `&'a dyn DbConnection` we hand out tied to `&'a Lua`.
    Ok(unsafe { &*ptr })
}

/// Removes the `TxContext` its scope installed — on the unwind path too.
///
/// A plain `remove_app_data` statement after the call is not enough. A
/// `TxContext` holds a fat pointer to a borrowed connection; if the call
/// unwinds, that statement is skipped, the connection drops (returning to
/// the pool), and the VM goes back to the pool still carrying a pointer to
/// freed memory. The next hook to run on that VM would dereference it.
///
/// Distinct from [`TxContextGuard`](crate::hooks::lifecycle::TxContextGuard),
/// which snapshots and restores the *whole* hook context (tx, user, locale,
/// infra). This one owns a single slot for the length of one CRUD call.
pub(crate) struct TxSlot<'a>(pub(crate) &'a Lua);

impl Drop for TxSlot<'_> {
    fn drop(&mut self) {
        self.0.remove_app_data::<TxContext>();
    }
}

/// Run `work` with a Lua context that has a `TxContext` set up,
/// dispatching on the active mode:
///
/// - **`TxContext`** already present → conn-mode pass-through. Just
///   calls `work` — the outer caller (hook runner, `crap.transaction`)
///   already installed the shared tx.
/// - **`PoolContext`** present → pool-mode. Opens a per-op `IMMEDIATE`
///   transaction with the full transaction scope (`run_scoped_tx`: the
///   same `crap.tx` queue, event gating, file cleanup, and cache
///   invalidation `crap.transaction(fn)` gets), runs `work` inside it, and
///   commits on `Ok` (or rolls back on `Err`).
/// - Neither set → returns a clear error.
///
/// `work` receives the same connection that's now visible to nested
/// `get_tx_conn(lua)` calls — so user code inside `work` can keep
/// using `get_tx_conn` unchanged. The `&dyn DbConnection` argument is
/// passed in case `work` wants to skip the indirection.
///
/// This is the helper that the `#[lua_fn(auto_tx)]` attribute wraps
/// every CRUD closure with: hook handlers use the outer shared tx;
/// job handlers (pool-mode) get a per-op IMMEDIATE tx without the
/// user fn knowing the difference.
///
/// # Errors
///
/// Returns a Lua runtime error if neither context is set, if the call
/// comes from an `after_read` hook, if the running job's deadline has
/// passed, or if pool acquisition / `BEGIN IMMEDIATE` / `COMMIT` fail.
pub(crate) fn with_lua_db<R>(
    lua: &Lua,
    work: impl FnOnce(&dyn DbConnection) -> LuaResult<R>,
) -> LuaResult<R> {
    // Checked BEFORE the conn-mode branch below, not after: in read-only
    // render mode `with_lua_db_read` installs a `TxContext` for the duration
    // of a read, and a `before_read` hook firing inside that read would
    // otherwise reach the pass-through and write on the read connection.
    ensure_writable(lua)?;
    refuse_in_after_read(lua)?;

    // A job past its timeout stops at its next database call — including one
    // that spends its time waiting on I/O and never trips the VM hook.
    check_execution_deadline(lua)?;

    // A hook's lazy transaction opens here, at its first write.
    open_lazy_tx_to_write(lua)?;

    // Conn-mode: a shared outer tx is already open. Hand the existing
    // connection to `work` — `get_tx_conn(lua)` inside `work` sees the
    // same TxContext — as one atomic step of that transaction.
    if lua.app_data_ref::<TxContext>().is_some() {
        let conn = get_tx_conn(lua)?;
        return run_step(lua, conn, work);
    }

    if lua.app_data_ref::<PoolContext>().is_none() {
        return Err(no_db_context());
    }

    // Pool-mode: a per-op transaction with the full scope, so a hook fired
    // by this single op sees the same `crap.tx` / event / file-cleanup
    // semantics as under `crap.transaction(fn)` or the service envelope.
    run_scoped_tx(lua, "crap.collections", work)
}

/// How far each of the scope's queues had grown when a step began — what a
/// failed step truncates them back to.
struct StepMarks {
    events: Option<usize>,
    verifications: Option<usize>,
    files: Option<usize>,
    deferred: Option<usize>,
}

impl StepMarks {
    fn take(lua: &Lua) -> Self {
        let infra = lua.app_data_ref::<LuaCrudInfra>();
        let infra = infra.as_deref();

        Self {
            events: infra
                .and_then(|i| i.event_queue.as_ref())
                .map(|q| q.borrow().len()),
            verifications: infra
                .and_then(|i| i.verification_queue.as_ref())
                .map(|q| q.borrow().len()),
            files: infra
                .and_then(|i| i.file_cleanup.as_ref())
                .map(|q| q.borrow().len()),
            deferred: infra
                .and_then(|i| i.deferred.as_ref())
                .map(|q| q.borrow().len()),
        }
    }

    /// The step was rolled back to its savepoint: what it queued goes with
    /// its writes — its events and verifications announce writes that never
    /// happened, and its file deletions would remove files a restored row
    /// still points at. Its `crap.tx.on_commit` effects are dropped for the
    /// same reason, while its `on_rollback` compensations are kept to run
    /// whatever the transaction's outcome: the step they compensate was
    /// undone either way.
    fn undo(self, lua: &Lua) {
        let Some(infra) = lua.app_data_ref::<LuaCrudInfra>().map(|r| (*r).clone()) else {
            return;
        };

        truncate(infra.event_queue.as_ref(), self.events);
        truncate(infra.verification_queue.as_ref(), self.verifications);
        truncate(infra.file_cleanup.as_ref(), self.files);

        let (Some(queue), Some(mark)) = (infra.deferred.as_ref(), self.deferred) else {
            return;
        };

        let mut queue = queue.borrow_mut();
        let start = mark.min(queue.len());
        let registered = queue.split_off(start);

        queue.extend(
            registered
                .into_iter()
                .filter(|e| e.outcome == EffectOutcome::Rollback)
                .map(|e| DeferredEffect {
                    unconditional: true,
                    ..e
                }),
        );
    }
}

/// Shorten `queue` back to `mark` entries.
fn truncate<T>(queue: Option<&Rc<RefCell<Vec<T>>>>, mark: Option<usize>) {
    if let (Some(queue), Some(mark)) = (queue, mark) {
        queue.borrow_mut().truncate(mark);
    }
}

/// Run one CRUD call (or a nested `crap.transaction` block) on the shared
/// transaction's connection as one atomic step of it (see
/// [`with_savepoint`]).
///
/// A step that fails — and a hook can catch the failure with `pcall` and
/// carry on — leaves none of its writes behind and none of what it queued
/// (see [`StepMarks::undo`]), while the enclosing transaction stays usable
/// and commits what the rest of the hook did. On Postgres a failed statement
/// otherwise aborts the whole transaction: its `COMMIT` would roll back every
/// write of the operation, the main document write included.
///
/// # Errors
///
/// Returns `work`'s error, or a Lua runtime error when the savepoint itself
/// fails.
pub(crate) fn run_step<R>(
    lua: &Lua,
    conn: &dyn DbConnection,
    work: impl FnOnce(&dyn DbConnection) -> LuaResult<R>,
) -> LuaResult<R> {
    // Outside a transaction every statement commits on its own: there is no
    // step to roll back, and nothing queued is undone.
    let marks = conn.in_transaction().then(|| StepMarks::take(lua));

    let result = with_savepoint(conn, || work(conn))
        .map_err(|e| RuntimeError(format!("{e:#}")))
        .and_then(|r| r);

    if let (Err(_), Some(marks)) = (&result, marks) {
        marks.undo(lua);
    }

    result
}

/// Refuse CRUD from inside an `after_read` hook.
///
/// `after_read` runs after the read's data is final and fails open (an error
/// is logged, the read succeeds). It has no transaction of its own on the
/// Rust-driven surfaces, and on a Lua-driven read it would write on the read's
/// transaction — which then commits even when the hook itself errored. The
/// contract is therefore "no CRUD in `after_read`" on every surface.
///
/// # Errors
///
/// Returns a Lua runtime error naming the alternative when an `after_read`
/// scope is active.
fn refuse_in_after_read(lua: &Lua) -> LuaResult<()> {
    if lua.app_data_ref::<AfterReadScope>().is_none() {
        return Ok(());
    }

    Err(RuntimeError(
        "crap.* CRUD is not available inside an after_read hook — it runs after the \
         read is final and fails open, so a write from it could half-apply. Do the \
         lookup in before_read (hand it over via ctx.context) or in the caller instead."
            .into(),
    ))
}

/// The read-only hook whose context is active, if any: an explicit
/// [`ReadOnlyScope`] (a read-only hook on a borrowed connection, such as the
/// `mfa_when` gate) or a [`PoolMode::ReadOnly`] pool context (the admin
/// `before_render` hook).
fn read_only_hook(lua: &Lua) -> Option<&'static str> {
    if let Some(scope) = lua.app_data_ref::<ReadOnlyScope>() {
        return Some(scope.0);
    }

    let ctx = lua.app_data_ref::<PoolContext>()?;

    (ctx.mode == PoolMode::ReadOnly).then_some("the admin `before_render` hook")
}

/// Refuse a write when the active context is read-only.
///
/// The gate for every read-only hook contract (the admin `before_render`
/// hook, the `mfa_when` gate). It deliberately looks at the read-only markers
/// **before** any `TxContext`, because a read-only context installs (or runs
/// on) a `TxContext` while a read is in flight — so a nested write (a
/// `before_read` hook running inside a render hook's `find`, for instance)
/// must still be refused rather than inheriting the read connection.
///
/// # Errors
///
/// Returns a Lua runtime error naming the read-only hook and the alternative
/// when a read-only context is active.
pub(crate) fn ensure_writable(lua: &Lua) -> LuaResult<()> {
    let Some(hook) = read_only_hook(lua) else {
        return Ok(());
    };

    Err(RuntimeError(format!(
        "this operation writes to the database, which is not available here — \
         {hook} runs read-only. Use the read functions (find, find_by_id, count, \
         ...) inside it, and do writes from a lifecycle hook, a job handler, or a \
         custom route instead."
    )))
}

/// The error raised when a CRUD function runs with neither a `TxContext`
/// nor a `PoolContext` installed.
fn no_db_context() -> mlua::Error {
    RuntimeError(
        "crap.collections CRUD functions require a transaction or pool \
         context (called inside a hook, a job handler, a custom route, an \
         admin `before_render` hook, or `crap.transaction(fn)`)"
            .into(),
    )
}

/// Read-capable counterpart of [`with_lua_db`], used by every read-only
/// CRUD function (`find`, `find_by_id`, `count`, `ref_count`, version
/// listing, `globals.get`, job-run reads).
///
/// Behaves identically to [`with_lua_db`] in conn-mode and in
/// [`PoolMode::Write`]. The difference is [`PoolMode::ReadOnly`] (the admin
/// `before_render` hook): instead of taking the write pool and a `BEGIN
/// IMMEDIATE`, it draws a **read**-pool connection and installs it directly
/// as the `TxContext`. A page render therefore never contends for the single
/// `SQLite` writer.
///
/// # Errors
///
/// Returns a Lua runtime error if no context is set, if the running job's
/// deadline has passed, or if pool acquisition / transaction handling fails.
pub(crate) fn with_lua_db_read<R>(
    lua: &Lua,
    work: impl FnOnce(&dyn DbConnection) -> LuaResult<R>,
) -> LuaResult<R> {
    refuse_in_after_read(lua)?;
    check_execution_deadline(lua)?;

    if let Some(reader) = lazy_reader(lua) {
        return read_on_lazy_reader(lua, reader, work);
    }

    open_lazy_tx(lua)?;

    if lua.app_data_ref::<TxContext>().is_some() {
        let conn = get_tx_conn(lua)?;
        return run_step(lua, conn, work);
    }

    let ctx = lua
        .app_data_ref::<PoolContext>()
        .ok_or_else(no_db_context)?;

    if ctx.mode == PoolMode::Write {
        drop(ctx);
        return with_lua_db(lua, work);
    }

    let pool = ctx.pool.clone();
    drop(ctx);

    let conn = pool
        .get()
        .map_err(|e| RuntimeError(format!("pool.get: {e}")))?;

    // SAFETY: `TxContext` stores a fat pointer to `&conn`. `conn` lives on
    // this function's stack and outlives the `work` call below; `TxSlot`
    // removes the pointer when the inner scope ends — including on unwind,
    // and always before `conn` drops back into the pool.
    lua.set_app_data(TxContext::new(&conn));

    let _slot = TxSlot(lua);

    work(&conn)
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use super::*;
    use crate::{
        config::CrapConfig,
        db::pool,
        hooks::lifecycle::{ExecutionDeadline, ExecutionDeadlineGuard, ReadOnlyScopeGuard},
    };
    use mlua::Lua;

    /// A throwaway pool over a temp-dir database — the tests below only need
    /// a `DbPool` value to hang a context off, not a migrated schema.
    fn test_pool() -> (tempfile::TempDir, crate::db::DbPool) {
        let dir = tempfile::TempDir::new().expect("tmpdir");
        let pool = pool::create_pool(dir.path(), &CrapConfig::default()).expect("pool");

        (dir, pool)
    }

    #[test]
    fn test_get_tx_conn_without_context() {
        let lua = Lua::new();
        let Err(err) = get_tx_conn(&lua) else {
            panic!("expected error when called outside hook context");
        };
        assert!(err.to_string().contains("need a database context"));
    }

    /// A write with no context at all names every place CRUD is legal,
    /// including the render hook.
    #[test]
    fn ensure_writable_is_a_no_op_without_a_pool_context() {
        let lua = Lua::new();

        assert!(
            ensure_writable(&lua).is_ok(),
            "no pool context means conn-mode or an error later — not a refusal here"
        );
    }

    #[test]
    fn ensure_writable_allows_a_write_mode_pool_context() {
        let lua = Lua::new();
        let (_dir, pool) = test_pool();
        lua.set_app_data(PoolContext {
            pool,
            mode: PoolMode::Write,
        });

        assert!(ensure_writable(&lua).is_ok());
    }

    #[test]
    fn ensure_writable_refuses_a_read_only_pool_context() {
        let lua = Lua::new();
        let (_dir, pool) = test_pool();
        lua.set_app_data(PoolContext {
            pool,
            mode: PoolMode::ReadOnly,
        });

        let Err(err) = ensure_writable(&lua) else {
            panic!("a read-only context must refuse writes");
        };
        assert!(
            err.to_string().contains("read-only"),
            "the message should name the read-only contract: {err}"
        );
    }

    /// A read-only hook running on a borrowed connection (the `mfa_when`
    /// gate) refuses writes by name — even with a `TxContext` installed —
    /// and the refusal lifts when its scope ends.
    #[test]
    fn ensure_writable_refuses_inside_a_read_only_scope() {
        let lua = Lua::new();

        {
            let _scope = ReadOnlyScopeGuard::install(&lua, "the `mfa_when` gate");

            let Err(err) = ensure_writable(&lua) else {
                panic!("a read-only scope must refuse writes");
            };
            assert!(
                err.to_string()
                    .contains("the `mfa_when` gate runs read-only"),
                "the message should name the read-only hook: {err}"
            );
        }

        assert!(
            ensure_writable(&lua).is_ok(),
            "the scope is removed when its guard drops"
        );
    }

    /// Regression: a Lua job past its timeout kept reading and writing — the
    /// scheduler could only record the timeout while the handler ran on. Its
    /// next database call, read or write, is now refused before any work.
    #[test]
    fn a_job_past_its_deadline_is_refused_at_its_next_database_call() {
        let lua = Lua::new();
        let (_dir, pool) = test_pool();
        lua.set_app_data(PoolContext {
            pool,
            mode: PoolMode::Write,
        });
        let _deadline = ExecutionDeadlineGuard::install(&lua, ExecutionDeadline::new(0));

        let write = with_lua_db(&lua, |_| -> LuaResult<()> {
            panic!("the write body must not run past the deadline")
        });
        let read = with_lua_db_read(&lua, |_| -> LuaResult<()> {
            panic!("the read body must not run past the deadline")
        });

        for result in [write, read] {
            let Err(err) = result else {
                panic!("a call past the deadline must be refused");
            };
            assert!(
                err.to_string().contains("exceeded its timeout"),
                "got: {err}"
            );
        }
    }

    /// Regression: a panic in the CRUD body must not leave a `TxContext`
    /// behind on the VM.
    ///
    /// The pointer inside it borrows a connection that drops as the stack
    /// unwinds, and the VM goes back to the pool — so a stale slot means the
    /// *next* hook to lease that VM dereferences freed memory.
    #[test]
    fn an_unwinding_crud_body_leaves_no_stale_tx_context() {
        let lua = Lua::new();
        let (_dir, pool) = test_pool();
        lua.set_app_data(PoolContext {
            pool,
            mode: PoolMode::ReadOnly,
        });

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = with_lua_db_read(&lua, |_| -> LuaResult<()> {
                panic!("CRUD body blew up");
            });
        }));

        assert!(panicked.is_err(), "the panic should propagate");
        assert!(
            lua.app_data_ref::<TxContext>().is_none(),
            "the TxContext must be removed on the unwind path, not just the happy path"
        );
    }

    /// Regression: the gate must be checked BEFORE the conn-mode
    /// pass-through, not after.
    ///
    /// `with_lua_db_read` installs a `TxContext` (the read-pool connection)
    /// for the duration of a read. Anything that runs *inside* that read —
    /// a nested hook, a field hook — would reach the conn-mode branch and
    /// inherit the read connection for a write, autocommitted, defeating
    /// the read-only contract from the inside. Ordering is the whole fix,
    /// so it gets its own test.
    #[test]
    fn a_read_only_context_refuses_a_write_even_with_a_tx_context_installed() {
        let lua = Lua::new();
        let (_dir, pool) = test_pool();
        lua.set_app_data(PoolContext {
            pool: pool.clone(),
            mode: PoolMode::ReadOnly,
        });

        let conn = pool.get().expect("connection");
        lua.set_app_data(TxContext::new(&conn));

        let result = with_lua_db(&lua, |_| Ok(()));

        let Err(err) = result else {
            panic!("the conn-mode pass-through must not bypass the read-only gate");
        };
        assert!(err.to_string().contains("read-only"), "got: {err}");
    }
}
