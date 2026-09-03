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
//! - On `Ok` return: remove `TxContext`, `COMMIT`, then run any
//!   `crap.tx.on_commit` effects registered inside the closure.
//! - On `Err` return: remove `TxContext`, drop the tx → automatic
//!   rollback, then run any `crap.tx.on_rollback` compensations.
//!
//! Inside a hook (already conn-mode with the parent's write tx),
//! `crap.transaction(fn)` is a pass-through: call `fn` directly so the
//! ops continue to share the outer tx. Nested explicit transactions
//! aren't supported (no `SAVEPOINT` mechanism in this alpha — defer
//! until a real use case surfaces).

use std::{cell::RefCell, rc::Rc};

use anyhow::Result;
use mlua::{Error::RuntimeError, Function, Lua, Result as LuaResult, Value};

use crate::{
    hooks::lifecycle::{LuaCrudInfra, PoolContext, TxContext, run_effects_on_vm},
    service::{DeferredEffect, DeferredQueue, EffectOutcome},
};

/// Wrap a Lua closure in a single IMMEDIATE transaction.
///
/// Errors out if called outside a job context (no `PoolContext` and no
/// `TxContext` — e.g., from `init.lua` or a top-level script).
#[allow(clippy::needless_pass_by_value)]
fn lua_transaction(lua: &Lua, fn_arg: Function) -> LuaResult<Value> {
    // Pass-through: already inside a shared tx (hook context, or a
    // surrounding `crap.transaction(fn)`). Call `fn` directly.
    if lua.app_data_ref::<TxContext>().is_some() {
        return fn_arg.call::<Value>(());
    }

    let pool = lua
        .app_data_ref::<PoolContext>()
        .ok_or_else(|| {
            RuntimeError(
                "crap.transaction() requires a job or pool context — call it from \
                 inside a Lua job handler, not from init.lua / collection definitions / \
                 top-level scripts"
                    .into(),
            )
        })?
        .0
        .clone();

    let mut conn = pool
        .write()
        .map_err(|e| RuntimeError(format!("crap.transaction: pool.write: {e}")))?;
    let tx = conn
        .transaction_immediate()
        .map_err(|e| RuntimeError(format!("crap.transaction: begin: {e}")))?;

    // Per-transaction queue for `crap.tx.on_commit` / `on_rollback`
    // registrations inside the closure. Installed by swapping a modified
    // `LuaCrudInfra` into app_data (snapshot/restore — the same stack
    // discipline as `TxContextGuard`).
    let dq: DeferredQueue = Rc::new(RefCell::new(Vec::new()));
    let prev_infra = lua.app_data_ref::<LuaCrudInfra>().map(|r| (*r).clone());

    let mut infra = prev_infra.clone().unwrap_or(LuaCrudInfra {
        event_transport: None,
        cache: None,
        event_queue: None,
        verification_queue: None,
        deferred: None,
    });
    infra.deferred = Some(dq.clone());
    lua.set_app_data(infra);

    // SAFETY: TxContext stores a fat pointer to `&tx`. `tx` lives on
    // this function's stack until just below — `remove_app_data` runs
    // before `tx.commit()` / drop, so the pointer is never
    // dereferenced after the tx is gone. Closure call runs
    // synchronously between set/remove.
    lua.set_app_data(TxContext::new(&tx));
    let call_result = fn_arg.call::<Value>(());
    lua.remove_app_data::<TxContext>();

    match prev_infra {
        Some(p) => {
            lua.set_app_data(p);
        }
        None => {
            lua.remove_app_data::<LuaCrudInfra>();
        }
    }

    let effects: Vec<DeferredEffect> = dq.borrow_mut().drain(..).collect();

    match call_result {
        Ok(value) => {
            if let Err(e) = tx.commit() {
                // A failed commit is a rollback outcome.
                run_effects_on_vm(lua, &effects, EffectOutcome::Rollback);

                return Err(RuntimeError(format!("crap.transaction: commit: {e}")));
            }

            // Effects run in THIS VM: `PoolContext` is live again (job
            // context), so effect CRUD is pool-mode, and events queue into
            // the job's own event queue (flushed post-handler).
            run_effects_on_vm(lua, &effects, EffectOutcome::Commit);

            Ok(value)
        }
        Err(e) => {
            // Roll back (and release the write lock) BEFORE compensations
            // run — their pool-mode CRUD needs the write path.
            drop(tx);
            run_effects_on_vm(lua, &effects, EffectOutcome::Rollback);

            Err(e)
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
