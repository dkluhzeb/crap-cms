//! Generic hook execution primitives: resolve hook references, call individual
//! hooks, dispatch collection-level + globally-registered handlers, and the
//! shared helpers consumed by sibling execution modules.

use std::{collections::HashSet, time::Instant};

use anyhow::{Context as _, Result, bail};
use mlua::{Function as LuaFunction, Lua, Table, Value};
use tracing::{debug, warn};

use crate::{
    core::{DocumentFields, HookRef, collection::Hooks},
    hooks::{
        lifecycle::{HookEvent, context::HookContext, converters::lua_table_to_json_map},
        lua_api,
    },
};

/// Runs collection-level hook refs, then global registered hooks.
/// `TxContext` must already be set in `app_data` if CRUD access is needed.
pub(crate) fn run_hooks_inner(
    lua: &Lua,
    hooks: &Hooks,
    event: HookEvent,
    mut context: HookContext,
) -> Result<HookContext> {
    let hook_refs = get_hook_refs(hooks, event);
    let timing = !hook_refs.is_empty() && tracing::enabled!(tracing::Level::DEBUG);
    let start = if timing { Some(Instant::now()) } else { None };

    for hook_ref in hook_refs {
        context = call_hook_ref(lua, hook_ref, context)?;
    }

    // Run global registered hooks
    context = call_registered_hooks(lua, event, context)?;

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
pub(crate) fn get_hook_refs(hooks: &Hooks, event: HookEvent) -> &[HookRef] {
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
/// that have at least one registered handler. Called once during `HookRunner::new()`.
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

/// Whether any `crap.template_data` function was registered by `init.lua`.
///
/// Scanned once at startup on a pre-warmed VM, like
/// [`scan_registered_events`] — every VM runs the same `init.lua`, and
/// registration is init-only. Lets the admin render path skip its blocking
/// hop when no Lua participates in rendering at all.
pub(crate) fn scan_has_template_data(lua: &Lua) -> bool {
    lua.named_registry_value::<Table>(crate::hooks::lua_api::template_data::TEMPLATE_DATA_KEY)
        .is_ok_and(|t| t.pairs::<Value, Value>().next().is_some())
}

/// Reuses the same context-to-table / table-to-context conversion as `call_hook_ref`.
pub(crate) fn call_registered_hooks(
    lua: &Lua,
    event: HookEvent,
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
            .with_context(|| format!("registered hook at index {i} is not a function"))?;

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

/// Read hook result data and context back from a returned Lua table into the `HookContext`.
pub(super) fn read_hook_result(ctx: &mut HookContext, tbl: &Table) -> Result<()> {
    if let Ok(data_tbl) = tbl.get::<Table>("data") {
        ctx.data = DocumentFields::from(lua_table_to_json_map(&data_tbl)?);
    }

    ctx.read_context_back(tbl)?;

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
        bail!("Hook ref '{hook_ref}' must be module.function format");
    }
    let module_path = parts[..parts.len() - 1].join(".");
    let func_name = parts[parts.len() - 1];

    let module: Table = require
        .call(module_path.clone())
        .with_context(|| format!("Failed to require module '{module_path}'"))?;
    let func: LuaFunction = module
        .get(func_name)
        .with_context(|| format!("Function '{func_name}' not found in module '{module_path}'"))?;

    Ok(func)
}

/// Resolve a dotted function reference (e.g., "`hooks.posts.auto_slug`")
/// and call it with the context.
pub(crate) fn call_hook_ref(
    lua: &Lua,
    hook: &HookRef,
    context: HookContext,
) -> Result<HookContext> {
    let hook_ref = hook.reference();
    let func = resolve_hook_function(lua, hook_ref)?;

    // Convert context to Lua table, injecting this ref's per-config `options`
    // (nil for a bare-string ref).
    let ctx_table = context.to_lua_table(lua)?;

    if let Some(options) = hook.options() {
        ctx_table.set("options", lua_api::json_to_lua(lua, options)?)?;
    }

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
    use serde_json::json;

    use super::*;

    /// A `{ ref, options }` hook config surfaces its `options` to the hook as
    /// `ctx.options`. Regression for the config-customizable-hooks feature: a
    /// bare-string ref leaves `ctx.options` nil, the table form injects it.
    #[test]
    fn call_hook_ref_injects_options() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"package.loaded["h"] = function(ctx)
                ctx.data.echoed = ctx.options and ctx.options.tag or "none"
                return ctx
            end"#,
        )
        .exec()
        .unwrap();

        // Bare ref → no options → hook sees nil.
        let bare = HookRef::new("h");
        let ctx = HookContext::builder("posts", "create").build();
        let out = call_hook_ref(&lua, &bare, ctx).unwrap();
        assert_eq!(out.data.get("echoed"), Some(&json!("none")));

        // `{ ref, options }` → hook sees ctx.options.tag.
        let parameterized = HookRef::with_options("h", json!({ "tag": "hello" }));
        let ctx = HookContext::builder("posts", "create").build();
        let out = call_hook_ref(&lua, &parameterized, ctx).unwrap();
        assert_eq!(out.data.get("echoed"), Some(&json!("hello")));
    }

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
            before_validate: vec![HookRef::new("hooks.validate")],
            before_change: vec![HookRef::new("hooks.change")],
            after_change: vec![HookRef::new("hooks.after")],
            before_read: vec![],
            after_read: vec![HookRef::new("hooks.read")],
            before_delete: vec![],
            after_delete: vec![],
            before_broadcast: vec![HookRef::new("hooks.broadcast")],
        };

        assert_eq!(
            get_hook_refs(&hooks, HookEvent::BeforeValidate),
            &[HookRef::new("hooks.validate")]
        );
        assert_eq!(
            get_hook_refs(&hooks, HookEvent::BeforeChange),
            &[HookRef::new("hooks.change")]
        );
        assert_eq!(
            get_hook_refs(&hooks, HookEvent::AfterChange),
            &[HookRef::new("hooks.after")]
        );
        assert!(get_hook_refs(&hooks, HookEvent::BeforeRead).is_empty());
        assert_eq!(
            get_hook_refs(&hooks, HookEvent::AfterRead),
            &[HookRef::new("hooks.read")]
        );
        assert!(get_hook_refs(&hooks, HookEvent::BeforeDelete).is_empty());
        assert!(get_hook_refs(&hooks, HookEvent::AfterDelete).is_empty());
        assert_eq!(
            get_hook_refs(&hooks, HookEvent::BeforeBroadcast),
            &[HookRef::new("hooks.broadcast")]
        );
        assert!(get_hook_refs(&hooks, HookEvent::BeforeRender).is_empty());
    }
}
