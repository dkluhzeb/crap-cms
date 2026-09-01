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
    hooks::lifecycle::{PoolContext, TxContext},
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
        .ok_or_else(|| {
            RuntimeError(
                "crap.collections CRUD functions require a transaction or pool \
                 context (called inside a hook, a job handler, or \
                 `crap.transaction(fn)`)"
                    .into(),
            )
        })?
        .0
        .clone();

    // Pool-mode always opens an IMMEDIATE tx (even for `find`/`count`), so
    // it is write-capable and must draw from the write pool.
    let mut conn = pool
        .write()
        .map_err(|e| RuntimeError(format!("pool.write: {e}")))?;
    let tx = conn
        .transaction_immediate()
        .map_err(|e| RuntimeError(format!("begin transaction: {e}")))?;

    // SAFETY: TxContext stores a fat-pointer to `&tx`. `tx` lives on
    // this function's stack and outlives the `work(&tx)` call below.
    // We `remove_app_data` before `tx.commit()` / drop, so the pointer
    // is never dereferenced after the tx is gone.
    lua.set_app_data(TxContext::new(&tx));
    let result = work(&tx);
    lua.remove_app_data::<TxContext>();

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
    use mlua::Lua;

    #[test]
    fn test_get_tx_conn_without_context() {
        let lua = Lua::new();
        let Err(err) = get_tx_conn(&lua) else {
            panic!("expected error when called outside hook context");
        };
        assert!(err.to_string().contains("need a database context"));
    }
}
