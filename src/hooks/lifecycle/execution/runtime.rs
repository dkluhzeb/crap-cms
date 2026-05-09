//! Generic hook execution primitives: resolve hook references, call individual
//! hooks, dispatch collection-level + globally-registered handlers, and the
//! shared helpers consumed by sibling execution modules.

use std::collections::HashSet;

use anyhow::{Context as _, Result, bail};
use mlua::{Function as LuaFunction, Lua, Table, Value};
use tracing::{debug, warn};

use crate::{
    core::{DocumentFields, collection::Hooks},
    hooks::{
        api,
        lifecycle::{HookEvent, context::HookContext},
    },
};

/// Runs collection-level hook refs, then global registered hooks.
/// TxContext must already be set in app_data if CRUD access is needed.
pub(crate) fn run_hooks_inner(
    lua: &Lua,
    hooks: &Hooks,
    event: HookEvent,
    mut context: HookContext,
) -> Result<HookContext> {
    let hook_refs = get_hook_refs(hooks, &event);
    let timing = !hook_refs.is_empty() && tracing::enabled!(tracing::Level::DEBUG);
    let start = if timing {
        Some(std::time::Instant::now())
    } else {
        None
    };

    for hook_ref in hook_refs {
        context = call_hook_ref(lua, hook_ref, context)?;
    }

    // Run global registered hooks
    context = call_registered_hooks(lua, &event, context)?;

    if let Some(start) = start {
        let elapsed = start.elapsed();

        debug!(
            "{} {event:?}: {} hook(s) in {:.2}ms",
            context.collection,
            hook_refs.len(),
            elapsed.as_secs_f64() * 1000.0,
        );
    }

    Ok(context)
}

/// Get the list of hook references for a given event.
pub(crate) fn get_hook_refs<'a>(hooks: &'a Hooks, event: &HookEvent) -> &'a [String] {
    match event {
        HookEvent::BeforeValidate => &hooks.before_validate,
        HookEvent::BeforeChange => &hooks.before_change,
        HookEvent::AfterChange => &hooks.after_change,
        HookEvent::BeforeRead => &hooks.before_read,
        HookEvent::AfterRead => &hooks.after_read,
        HookEvent::BeforeDelete => &hooks.before_delete,
        HookEvent::AfterDelete => &hooks.after_delete,
        HookEvent::BeforeBroadcast => &hooks.before_broadcast,
        HookEvent::BeforeRender => &[], // global-only, no collection-level refs
    }
}

/// Check if any globally registered hooks exist for the given event.
pub(crate) fn has_registered_hooks(lua: &Lua, event: &str) -> bool {
    let event_hooks: Table = match lua.named_registry_value("_crap_event_hooks") {
        Ok(t) => t,
        Err(_) => return false,
    };

    match event_hooks.get::<Value>(event) {
        Ok(Value::Table(t)) => t.raw_len() > 0,
        _ => false,
    }
}

/// Scan a Lua VM's `_crap_event_hooks` table and return the set of event names
/// that have at least one registered handler. Called once during HookRunner::new().
pub(crate) fn scan_registered_events(lua: &Lua) -> HashSet<String> {
    let mut events = HashSet::new();
    let event_hooks: Table = match lua.named_registry_value("_crap_event_hooks") {
        Ok(t) => t,
        Err(_) => return events,
    };

    for pair in event_hooks.pairs::<String, Value>() {
        if let Ok((key, Value::Table(t))) = pair
            && t.raw_len() > 0
        {
            events.insert(key);
        }
    }

    events
}

/// Reuses the same context-to-table / table-to-context conversion as `call_hook_ref`.
pub(crate) fn call_registered_hooks(
    lua: &Lua,
    event: &HookEvent,
    mut context: HookContext,
) -> Result<HookContext> {
    let event_hooks: Table = match lua.named_registry_value("_crap_event_hooks") {
        Ok(t) => t,
        Err(_) => return Ok(context),
    };

    let list: Table = match event_hooks.get::<Value>(event.as_str()) {
        Ok(Value::Table(t)) => t,
        _ => return Ok(context),
    };

    let len = list.raw_len();

    if len == 0 {
        return Ok(context);
    }

    for i in 1..=len {
        let func: LuaFunction = list
            .raw_get(i)
            .with_context(|| format!("registered hook at index {} is not a function", i))?;

        debug!(
            "Running registered {} hook #{} for {}",
            event.as_str(),
            i,
            context.collection
        );

        let ctx_table = context.to_lua_table(lua)?;

        let result: Value = func.call(ctx_table)?;

        if let Value::Table(tbl) = &result {
            read_hook_result(&mut context, tbl)?;
        } else if !matches!(result, Value::Nil) {
            warn!(
                "Registered {} hook #{} for {} returned {} instead of a table — ignoring",
                event.as_str(),
                i,
                context.collection,
                result.type_name()
            );
        }
    }

    Ok(context)
}

/// Read hook result data and context back from a returned Lua table into the HookContext.
pub(super) fn read_hook_result(ctx: &mut HookContext, tbl: &Table) -> Result<()> {
    if let Ok(data_tbl) = tbl.get::<Table>("data") {
        let mut new_data = DocumentFields::new();

        for pair in data_tbl.pairs::<String, Value>() {
            let (k, v) = pair?;
            new_data.insert(k, api::lua_to_json(&v)?);
        }

        ctx.data = new_data;
    }

    ctx.read_context_back(tbl);

    Ok(())
}

/// Resolve a hook reference to a Lua function.
///
/// Tries file-per-hook first: `require("hooks.posts.auto_slug")` → function.
/// Falls back to module pattern: `require("hooks.posts")["auto_slug"]`.
pub(crate) fn resolve_hook_function(lua: &Lua, hook_ref: &str) -> Result<LuaFunction> {
    let require: LuaFunction = lua.globals().get("require")?;

    // Try file-per-hook: require("hooks.posts.auto_slug") → function
    if let Ok(Value::Function(f)) = require.call::<Value>(hook_ref) {
        return Ok(f);
    }

    // Fallback: module.function pattern
    let parts: Vec<&str> = hook_ref.split('.').collect();

    if parts.len() < 2 {
        bail!("Hook ref '{}' must be module.function format", hook_ref);
    }
    let module_path = parts[..parts.len() - 1].join(".");
    let func_name = parts[parts.len() - 1];

    let module: Table = require
        .call(module_path.clone())
        .with_context(|| format!("Failed to require module '{}'", module_path))?;
    let func: LuaFunction = module.get(func_name).with_context(|| {
        format!(
            "Function '{}' not found in module '{}'",
            func_name, module_path
        )
    })?;

    Ok(func)
}

/// Resolve a dotted function reference (e.g., "hooks.posts.auto_slug")
/// and call it with the context.
pub(crate) fn call_hook_ref(
    lua: &Lua,
    hook_ref: &str,
    context: HookContext,
) -> Result<HookContext> {
    let func = resolve_hook_function(lua, hook_ref)?;

    // Convert context to Lua table
    let ctx_table = context.to_lua_table(lua)?;

    // Call the hook with optional timing (zero-cost when debug is disabled)
    let timing = tracing::enabled!(tracing::Level::DEBUG);
    let start = if timing {
        Some(std::time::Instant::now())
    } else {
        None
    };
    let result: Value = func.call(ctx_table)?;

    if let Some(start) = start {
        debug!(
            "{}: {:.2}ms",
            hook_ref,
            start.elapsed().as_secs_f64() * 1000.0
        );
    }

    // Parse the result back
    match result {
        Value::Table(tbl) => {
            let mut ctx = context;

            read_hook_result(&mut ctx, &tbl)?;

            Ok(ctx)
        }
        Value::Nil => Ok(context),
        other => {
            warn!(
                "Hook '{}' for {} returned {} instead of a table — ignoring return value",
                hook_ref,
                context.collection,
                other.type_name()
            );

            Ok(context)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_registered_hooks_empty() {
        let lua = mlua::Lua::new();
        lua.set_named_registry_value("_crap_event_hooks", lua.create_table().unwrap())
            .unwrap();
        assert!(!has_registered_hooks(&lua, "after_read"));
    }

    #[test]
    fn has_registered_hooks_with_hooks() {
        let lua = mlua::Lua::new();
        let event_hooks = lua.create_table().unwrap();
        let after_read_list = lua.create_table().unwrap();
        let noop_fn = lua.create_function(|_, ()| Ok(())).unwrap();
        after_read_list.set(1, noop_fn).unwrap();
        event_hooks.set("after_read", after_read_list).unwrap();
        lua.set_named_registry_value("_crap_event_hooks", event_hooks)
            .unwrap();
        assert!(has_registered_hooks(&lua, "after_read"));
        assert!(!has_registered_hooks(&lua, "before_change"));
    }

    #[test]
    fn has_registered_hooks_no_global() {
        let lua = mlua::Lua::new();
        assert!(!has_registered_hooks(&lua, "after_read"));
    }

    #[test]
    fn get_hook_refs_maps_events() {
        let hooks = Hooks {
            before_validate: vec!["hooks.validate".to_string()],
            before_change: vec!["hooks.change".to_string()],
            after_change: vec!["hooks.after".to_string()],
            before_read: vec![],
            after_read: vec!["hooks.read".to_string()],
            before_delete: vec![],
            after_delete: vec![],
            before_broadcast: vec!["hooks.broadcast".to_string()],
        };

        assert_eq!(
            get_hook_refs(&hooks, &HookEvent::BeforeValidate),
            &["hooks.validate"]
        );
        assert_eq!(
            get_hook_refs(&hooks, &HookEvent::BeforeChange),
            &["hooks.change"]
        );
        assert_eq!(
            get_hook_refs(&hooks, &HookEvent::AfterChange),
            &["hooks.after"]
        );
        assert!(get_hook_refs(&hooks, &HookEvent::BeforeRead).is_empty());
        assert_eq!(
            get_hook_refs(&hooks, &HookEvent::AfterRead),
            &["hooks.read"]
        );
        assert!(get_hook_refs(&hooks, &HookEvent::BeforeDelete).is_empty());
        assert!(get_hook_refs(&hooks, &HookEvent::AfterDelete).is_empty());
        assert_eq!(
            get_hook_refs(&hooks, &HookEvent::BeforeBroadcast),
            &["hooks.broadcast"]
        );
        assert!(get_hook_refs(&hooks, &HookEvent::BeforeRender).is_empty());
    }
}
