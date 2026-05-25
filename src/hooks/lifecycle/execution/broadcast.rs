//! Before-broadcast hook variants. These hooks run when a mutation event is
//! about to ship to live-update subscribers, giving collection-level and
//! globally-registered handlers a chance to mutate or veto the broadcast.

use anyhow::{Context as _, Result};
use mlua::{Function as LuaFunction, Lua, Table, Value};
use tracing::warn;

use crate::hooks::lifecycle::context::HookContext;

use super::runtime::{read_hook_result, resolve_hook_function};

/// Call a `before_broadcast` hook ref. Returns Some(context) to continue, None to suppress.
pub(crate) fn call_before_broadcast_hook(
    lua: &Lua,
    hook_ref: &str,
    context: HookContext,
) -> Result<Option<HookContext>> {
    let func = resolve_hook_function(lua, hook_ref)?;

    let ctx_table = context.to_lua_table(lua)?;
    let result: Value = func.call(ctx_table)?;

    match result {
        Value::Boolean(false) | Value::Nil => Ok(None),
        Value::Table(tbl) => {
            let mut ctx = context;

            read_hook_result(&mut ctx, &tbl)?;

            Ok(Some(ctx))
        }
        other => {
            warn!(
                "before_broadcast hook '{}' returned {} instead of table/false/nil — ignoring",
                hook_ref,
                other.type_name()
            );

            Ok(Some(context))
        }
    }
}

/// Call all globally registered `before_broadcast` hooks.
/// Returns Some(context) to continue, None if any hook suppresses.
pub(crate) fn call_registered_before_broadcast(
    lua: &Lua,
    mut context: HookContext,
) -> Result<Option<HookContext>> {
    let event_hooks: Table = match lua.named_registry_value("_crap_event_hooks") {
        Ok(t) => t,
        Err(_) => return Ok(Some(context)),
    };

    let list: Table = match event_hooks.get::<Value>("before_broadcast") {
        Ok(Value::Table(t)) => t,
        _ => return Ok(Some(context)),
    };

    let len = list.raw_len();

    if len == 0 {
        return Ok(Some(context));
    }

    for i in 1..=len {
        let func: LuaFunction = list.raw_get(i).with_context(|| {
            format!("registered before_broadcast hook at index {i} is not a function")
        })?;

        let ctx_table = context.to_lua_table(lua)?;

        let result: Value = func.call(ctx_table)?;

        match result {
            Value::Boolean(false) | Value::Nil => return Ok(None),
            Value::Table(tbl) => {
                read_hook_result(&mut context, &tbl)?;
            }
            other => {
                warn!(
                    "Registered before_broadcast hook #{} returned {} instead of table/false/nil — ignoring",
                    i,
                    other.type_name()
                );
            }
        }
    }

    Ok(Some(context))
}
