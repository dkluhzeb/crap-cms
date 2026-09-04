//! Helpers for retrieving the active DB connection from Lua `app_data`.
//! Used by every Lua CRUD closure. Two dispatch modes:
//!
//! - **Conn-mode** (hooks, `crap.transaction(fn)`): a single `TxContext`
//!   is set in `app_data` for the duration of the outer call; every CRUD
//!   op uses that shared transaction.
//! - **Pool-mode** (job handlers): a `PoolContext` is set instead, and
//!   each CRUD op opens its own short-lived `IMMEDIATE` transaction via
//!   the pool. Avoids the `SQLITE_BUSY_SNAPSHOT` hazard that fires when
//!   a long-running handler's read snapshot collides with concurrent
//!   writers.
//!
//! Callers should use [`with_lua_db`] which handles both modes uniformly;
//! [`get_tx_conn`] is the conn-mode-only path retained for hook-internal
//! code that knows the mode.

use mlua::{Error::RuntimeError, Lua, Result as LuaResult};

use crate::{
    db::DbConnection,
    hooks::lifecycle::{PoolContext, PoolMode, TxContext},
};

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
/// - **`PoolContext`** present → pool-mode. Pulls a fresh connection,
///   opens an `IMMEDIATE` transaction, **installs the tx as
///   `TxContext`** for the duration of `work`, runs `work`, removes
///   the `TxContext`, and commits on `Ok` (or rolls back on `Err`).
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
/// Returns a Lua runtime error if neither context is set, or if pool
/// acquisition / `BEGIN IMMEDIATE` / `COMMIT` fail.
pub(crate) fn with_lua_db<R>(
    lua: &Lua,
    work: impl FnOnce(&dyn DbConnection) -> LuaResult<R>,
) -> LuaResult<R> {
    // Checked BEFORE the conn-mode branch below, not after: in read-only
    // render mode `with_lua_db_read` installs a `TxContext` for the duration
    // of a read, and a `before_read` hook firing inside that read would
    // otherwise reach the pass-through and write on the read connection.
    ensure_writable(lua)?;

    // Conn-mode: a shared outer tx is already open. Hand the existing
    // connection to `work` — `get_tx_conn(lua)` inside `work` sees the
    // same TxContext.
    if lua.app_data_ref::<TxContext>().is_some() {
        let conn = get_tx_conn(lua)?;
        return work(conn);
    }

    // Pool-mode: open a short-lived IMMEDIATE tx for this single op
    // and install it as `TxContext` so existing `get_tx_conn` users
    // inside `work` continue to work transparently.
    let pool = lua
        .app_data_ref::<PoolContext>()
        .ok_or_else(no_db_context)?
        .pool
        .clone();

    // Pool-mode always opens an IMMEDIATE tx (even for `find`/`count`), so
    // it is write-capable and must draw from the write pool.
    let mut conn = pool
        .write()
        .map_err(|e| RuntimeError(format!("pool.write: {e}")))?;
    let tx = conn
        .transaction_immediate()
        .map_err(|e| RuntimeError(format!("begin transaction: {e}")))?;

    // SAFETY: TxContext stores a fat-pointer to `&tx`. `tx` lives on this
    // function's stack and outlives the `work(&tx)` call below. `TxSlot`
    // removes the pointer when the inner scope ends — including on unwind,
    // and always before `tx` is committed or dropped.
    lua.set_app_data(TxContext::new(&tx));
    let result = {
        let _slot = TxSlot(lua);

        work(&tx)
    };

    match result {
        Ok(value) => {
            tx.commit()
                .map_err(|e| RuntimeError(format!("commit transaction: {e}")))?;
            Ok(value)
        }
        Err(e) => {
            // `tx` drops here → automatic rollback.
            Err(e)
        }
    }
}

/// Refuse a write when the active context is read-only.
///
/// The gate for the admin `before_render` contract. It deliberately looks at
/// the `PoolContext` **before** any `TxContext`, because a read-only context
/// installs a `TxContext` while a read is in flight — so a nested write (a
/// `before_read` hook running inside a render hook's `find`, for instance)
/// must still be refused rather than inheriting the read connection.
///
/// # Errors
///
/// Returns a Lua runtime error naming the alternative when the active pool
/// context is [`PoolMode::ReadOnly`].
pub(crate) fn ensure_writable(lua: &Lua) -> LuaResult<()> {
    let Some(ctx) = lua.app_data_ref::<PoolContext>() else {
        return Ok(());
    };

    if ctx.mode == PoolMode::ReadOnly {
        return Err(RuntimeError(
            "this operation writes to the database, which is not available here — \
             the admin `before_render` hook runs read-only. Use the read functions \
             (find, find_by_id, count, ...) to build page data, and do writes from \
             a lifecycle hook, a job handler, or a custom route instead."
                .into(),
        ));
    }

    Ok(())
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
/// Returns a Lua runtime error if no context is set, or if pool acquisition
/// / transaction handling fails.
pub(crate) fn with_lua_db_read<R>(
    lua: &Lua,
    work: impl FnOnce(&dyn DbConnection) -> LuaResult<R>,
) -> LuaResult<R> {
    if lua.app_data_ref::<TxContext>().is_some() {
        let conn = get_tx_conn(lua)?;
        return work(conn);
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
    use crate::{config::CrapConfig, db::pool};
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
    /// pass-through, not after.    /// Regression: the gate must be checked BEFORE the conn-mode
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
