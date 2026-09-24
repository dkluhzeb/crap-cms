//! Register `crap.tx.on_commit(ref, data?)` / `crap.tx.on_rollback(ref, data?)`
//! — transaction-outcome effects for hook side effects.
//!
//! A lifecycle hook runs **inside** the write transaction, but its external
//! side effects (HTTP calls, emails via custom code, third-party APIs) are
//! not transactional: they fire even when the transaction later rolls back,
//! and they fire before the data is durable. `crap.tx.on_commit` defers a
//! side effect until the transaction actually committed; `crap.tx.on_rollback`
//! registers a compensation that runs only if it rolled back.
//!
//! Effects are **hook refs + plain data** (`crap.tx.on_commit("hooks.x.y",
//! { id = ctx.id })`), not closures — the effect runs later in a pooled VM
//! (pool-mode, like a job handler), so a closure from the registering VM
//! could not travel. This matches every other handler surface (routes, jobs,
//! `mfa_deliver`). Registration is validated fail-closed: an unresolvable
//! ref or an unserializable payload raises in the registering hook, rolling
//! the transaction back. Effect execution is fail-open: errors are logged,
//! the outcome is already final.

use anyhow::{Context as _, Result};
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};
use serde_json::Value as JsonValue;

use crate::{
    hooks::lifecycle::{LuaCrudInfra, resolve_hook_function},
    service::{DeferredEffect, DeferredQueue, EffectOutcome},
};

/// Fetch the active deferred queue, or error with guidance when there is no
/// enclosing transaction to attach to.
fn active_queue(lua: &Lua, name: &str) -> LuaResult<DeferredQueue> {
    let queue = lua
        .app_data_ref::<LuaCrudInfra>()
        .and_then(|infra| infra.deferred.clone());

    queue.ok_or_else(|| {
        RuntimeError(format!(
            "crap.tx.{name} requires an active write transaction — call it \
             from a write lifecycle hook (before_validate, before_change, \
             after_change, before_delete, after_delete) or inside \
             crap.transaction(fn)"
        ))
    })
}

/// Shared body for both registration functions.
fn register_effect(
    lua: &Lua,
    name: &str,
    outcome: EffectOutcome,
    hook_ref: &str,
    data: Option<&Value>,
) -> LuaResult<()> {
    let queue = active_queue(lua, name)?;

    // Fail-closed validation at registration time: a bad ref or payload
    // aborts the registering hook (and with it the transaction).
    resolve_hook_function(lua, hook_ref)
        .map_err(|e| RuntimeError(format!("crap.tx.{name}: hook ref '{hook_ref}': {e:#}")))?;

    // `data` is a table or nothing — the documented `table?` contract. A
    // bare string/number is refused rather than accepted as a payload the
    // handler's `ctx.data` was never documented to carry.
    let payload = match data {
        None | Some(Value::Nil) => JsonValue::Null,
        Some(v @ Value::Table(_)) => super::lua_to_json(v)
            .map_err(|e| RuntimeError(format!("crap.tx.{name}: data not serializable: {e}")))?,
        Some(other) => {
            return Err(RuntimeError(format!(
                "crap.tx.{name}: data must be a table (or nil), got {}",
                other.type_name()
            )));
        }
    };

    queue.borrow_mut().push(DeferredEffect {
        outcome,
        hook_ref: hook_ref.to_string(),
        payload,
    });

    Ok(())
}

/// Register the `crap.tx` table on the given VM.
///
/// # Errors
///
/// Returns an error if table/function creation or assignment fails.
pub(crate) fn register_tx_hooks(lua: &Lua) -> Result<()> {
    let crap: Table = lua.globals().get("crap").context("crap table missing")?;
    let tx = lua.create_table()?;

    let on_commit = lua.create_function(|lua, (hook_ref, data): (String, Option<Value>)| {
        register_effect(
            lua,
            "on_commit",
            EffectOutcome::Commit,
            &hook_ref,
            data.as_ref(),
        )
    })?;
    tx.set("on_commit", on_commit)?;

    let on_rollback = lua.create_function(|lua, (hook_ref, data): (String, Option<Value>)| {
        register_effect(
            lua,
            "on_rollback",
            EffectOutcome::Rollback,
            &hook_ref,
            data.as_ref(),
        )
    })?;
    tx.set("on_rollback", on_rollback)?;

    crap.set("tx", tx)?;

    Ok(())
}

/// Render `crap.tx.*` into the generated `types/crap.lua`. Hand-written
/// because the payload is an open table (`data?: table`) and the functions
/// are registration-only (no return value worth typing).
pub(crate) fn render_crap_tx_lua(out: &mut String) {
    out.push_str(
        "\
-- ── crap.tx — transaction-outcome effects ──────────────────────────

--- @class crap.tx
crap.tx = {}

--- Defer a side effect until the surrounding write transaction has
--- **committed**. `ref` is a hook reference (`\"hooks.module.fn\"`);
--- `data` is captured immediately (plain, JSON-serializable values
--- only) and passed to the handler as `ctx.data` together with
--- `ctx.outcome = \"commit\"`. The handler runs post-commit in
--- pool-mode: CRUD works, each call in its own short transaction.
--- Handler errors are logged, never propagated (the commit already
--- happened). Registering with an unresolvable ref or an
--- unserializable payload raises immediately, rolling the
--- transaction back.
---
--- Only valid inside a write lifecycle hook or `crap.transaction(fn)`.
---
--- @param ref string
--- @param data? table
function crap.tx.on_commit(ref, data) end

--- Register a compensation that runs only if the surrounding write
--- transaction **rolls back** (hook error, validation failure, or
--- failed commit). Same contract as `crap.tx.on_commit`, with
--- `ctx.outcome = \"rollback\"`.
---
--- @param ref string
--- @param data? table
function crap.tx.on_rollback(ref, data) end

",
    );
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use serde_json::json;

    use super::*;

    /// A VM with `crap.tx` registered, an active deferred queue, and a
    /// resolvable `hooks.x` ref.
    fn lua_with_queue() -> (Lua, DeferredQueue) {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_tx_hooks(&lua).unwrap();
        lua.load(r#"package.loaded["hooks.x"] = function(ctx) end"#)
            .exec()
            .unwrap();

        let queue: DeferredQueue = Rc::new(RefCell::new(Vec::new()));
        lua.set_app_data(LuaCrudInfra {
            deferred: Some(queue.clone()),
            ..LuaCrudInfra::default()
        });

        (lua, queue)
    }

    /// `data` is typed `table?`; a string used to be accepted as a payload.
    #[test]
    fn non_table_data_is_rejected() {
        let (lua, queue) = lua_with_queue();

        let err = lua
            .load(r#"crap.tx.on_commit("hooks.x", "str")"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("data must be a table"), "{err}");
        assert!(err.contains("got string"), "{err}");

        let err = lua
            .load(r#"crap.tx.on_rollback("hooks.x", 42)"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("crap.tx.on_rollback: data must be a table"),
            "{err}"
        );

        assert!(
            queue.borrow().is_empty(),
            "a refused registration queues nothing"
        );
    }

    #[test]
    fn table_and_absent_data_register_the_effect() {
        let (lua, queue) = lua_with_queue();

        lua.load(
            r#"
            crap.tx.on_commit("hooks.x", { id = "d1" })
            crap.tx.on_commit("hooks.x")
            "#,
        )
        .exec()
        .unwrap();

        let effects = queue.borrow();
        assert_eq!(effects.len(), 2);
        assert_eq!(effects[0].payload, json!({ "id": "d1" }));
        assert_eq!(effects[1].payload, JsonValue::Null);
        assert_eq!(effects[0].hook_ref, "hooks.x");
    }
}
