//! Register `crap.hooks` — register/remove global event hooks, plus `_crap_event_hooks` storage.

use anyhow::Result;
use mlua::{Error::RuntimeError, Function, Lua, Result as LuaResult, Table, Value};

use super::utils::require_init_phase;
use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Known lifecycle event names. Registering any other event name is a hard error.
const KNOWN_EVENTS: &[&str] = &[
    "before_validate",
    "before_change",
    "after_change",
    "before_read",
    "after_read",
    "before_delete",
    "after_delete",
    "before_broadcast",
    "before_render",
];

fn is_known_event(event: &str) -> bool {
    KNOWN_EVENTS.contains(&event)
}

/// Register a hook function for an event. Fires for all collections.
#[lua_fn(path = "crap.hooks.register")]
fn hooks_register(
    lua: &Lua,
    #[lua(ty = "crap.HookEvent", doc = "The lifecycle event to hook into.")] event: String,
    #[lua(
        ty = "fun(context: crap.HookContext): crap.HookContext",
        doc = "Hook function."
    )]
    func: Function,
) -> LuaResult<()> {
    require_init_phase(
        lua,
        "crap.hooks.register must be called from init.lua or a definition \
         file — runtime registration only lands in one VM of the pool and \
         is intermittent across requests",
    )?;

    if !is_known_event(&event) {
        return Err(RuntimeError(format!(
            "crap.hooks.register: unknown event '{event}'. Known events: {}",
            KNOWN_EVENTS.join(", ")
        )));
    }
    let list = get_or_create_hook_list(lua, &event)?;
    list.set(list.raw_len() + 1, func)
}

/// Remove a previously registered hook function (identity-based via rawequal).
#[lua_fn(path = "crap.hooks.remove")]
fn hooks_remove(
    lua: &Lua,
    #[lua(ty = "crap.HookEvent", doc = "The lifecycle event.")] event: String,
    #[lua(
        ty = "fun(context: crap.HookContext): crap.HookContext",
        doc = "The function to remove."
    )]
    func: Function,
) -> LuaResult<()> {
    require_init_phase(
        lua,
        "crap.hooks.remove must be called from init.lua or a definition \
         file — runtime removal only affects one VM of the pool and is \
         intermittent across requests",
    )?;

    if !is_known_event(&event) {
        return Err(RuntimeError(format!(
            "crap.hooks.remove: unknown event '{event}'. Known events: {}",
            KNOWN_EVENTS.join(", ")
        )));
    }

    let event_hooks: Table = lua.named_registry_value("_crap_event_hooks")?;
    let Value::Table(list) = event_hooks.get::<Value>(event.as_str())? else {
        return Ok(());
    };

    let rawequal: Function = lua.globals().get("rawequal")?;

    for i in 1..=list.raw_len() {
        let entry: Value = list.raw_get(i)?;

        if rawequal.call::<bool>((entry, func.clone()))? {
            let table_remove: Function = lua.load("table.remove").eval()?;
            table_remove.call::<()>((list, i))?;

            break;
        }
    }

    Ok(())
}

/// List all registered hook functions for an event.
#[lua_fn(
    path = "crap.hooks.list",
    returns = "fun(context: crap.HookContext): crap.HookContext[]",
    returns_doc = "Array of hook functions."
)]
fn hooks_list(
    lua: &Lua,
    #[lua(ty = "crap.HookEvent", doc = "The lifecycle event.")] event: String,
) -> LuaResult<Table> {
    let event_hooks: Table = lua.named_registry_value("_crap_event_hooks")?;

    match event_hooks.get::<Value>(event.as_str())? {
        Value::Table(t) => Ok(t),
        _ => lua.create_table(),
    }
}

lua_table! {
    name: crap_hooks,
    path: "crap.hooks",
    state: (),
    header: "Hook registration API.",
    fns: [hooks_register, hooks_remove, hooks_list],
}

/// Register `crap.hooks.register`, `crap.hooks.remove`, and
/// `crap.hooks.list`, plus the `_crap_event_hooks` storage table.
/// Parent `crap` must already be in globals.
pub(super) fn register_hooks(lua: &Lua) -> Result<()> {
    lua.set_named_registry_value("_crap_event_hooks", lua.create_table()?)?;
    register_crap_hooks(lua, ())?;
    Ok(())
}

/// Get the event hook list for an event, creating it if absent.
fn get_or_create_hook_list(lua: &Lua, event: &str) -> LuaResult<Table> {
    let event_hooks: Table = lua.named_registry_value("_crap_event_hooks")?;

    if let Value::Table(t) = event_hooks.get::<Value>(event)? {
        Ok(t)
    } else {
        let t = lua.create_table()?;
        event_hooks.set(event, t.clone())?;
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::lifecycle::InitPhase;

    /// a runtime `crap.hooks.register`/`remove`
    /// (no `InitPhase` marker) must be rejected — it would land in one
    /// pooled VM and fire intermittently across requests.
    #[test]
    fn register_and_remove_are_rejected_outside_init_phase() {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_hooks(&lua).unwrap();

        let err = lua
            .load(r#"crap.hooks.register("before_change", function(c) return c end)"#)
            .exec()
            .unwrap_err();
        assert!(
            err.to_string().contains("init.lua"),
            "runtime register must name the init-phase rule: {err}"
        );

        let err = lua
            .load(r#"crap.hooks.remove("before_change", function(c) return c end)"#)
            .exec()
            .unwrap_err();
        assert!(err.to_string().contains("init.lua"), "{err}");

        // With the marker set, registration works.
        lua.set_app_data(InitPhase);
        lua.load(r#"crap.hooks.register("before_change", function(c) return c end)"#)
            .exec()
            .unwrap();
    }

    /// Removing a function that was never registered is a no-op.
    #[test]
    fn remove_function_not_in_list_is_noop() {
        let lua = lua_with_hooks();
        lua.load(
            r#"
            crap.hooks.register("before_change", function(c) return c end)
            crap.hooks.remove("before_change", function(c) return c end)
            assert(#crap.hooks.list("before_change") == 1)
        "#,
        )
        .exec()
        .unwrap();
    }

    fn lua_with_hooks() -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_hooks(&lua).unwrap();
        lua.set_app_data(InitPhase);
        lua
    }

    fn crap_hooks(lua: &Lua) -> Table {
        let crap: Table = lua.globals().get("crap").unwrap();
        crap.get("hooks").unwrap()
    }

    #[test]
    fn test_register_hooks_remove_nonexistent_event_is_noop() {
        let lua = lua_with_hooks();
        let remove_fn: Function = crap_hooks(&lua).get("remove").unwrap();
        let dummy_fn = lua.create_function(|_, ()| Ok(())).unwrap();
        let result: LuaResult<()> = remove_fn.call(("before_change", dummy_fn));
        assert!(
            result.is_ok(),
            "remove on nonexistent event should be a noop"
        );
    }

    /// Regression: `remove` accepted an UNKNOWN event name as a silent
    /// no-op — the same typo class `register` guards against. It must error.
    #[test]
    fn remove_unknown_event_name_is_rejected() {
        let lua = lua_with_hooks();
        let remove_fn: Function = crap_hooks(&lua).get("remove").unwrap();
        let dummy_fn = lua.create_function(|_, ()| Ok(())).unwrap();
        let result: LuaResult<()> = remove_fn.call(("after_chnage", dummy_fn));
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown event"),
            "remove on an unknown event must error, got: {err}"
        );
    }

    #[test]
    fn test_register_hooks_register_and_remove() {
        let lua = lua_with_hooks();
        let hooks = crap_hooks(&lua);
        let register_fn: Function = hooks.get("register").unwrap();
        let remove_fn: Function = hooks.get("remove").unwrap();

        let hook_fn = lua.create_function(|_, ()| Ok(())).unwrap();
        let _: () = register_fn.call(("after_change", hook_fn.clone())).unwrap();

        let event_hooks: Table = lua.named_registry_value("_crap_event_hooks").unwrap();
        let list: Table = event_hooks.get("after_change").unwrap();
        assert_eq!(list.raw_len(), 1);

        let _: () = remove_fn.call(("after_change", hook_fn)).unwrap();
        let event_hooks: Table = lua.named_registry_value("_crap_event_hooks").unwrap();
        let list_after: Table = event_hooks.get("after_change").unwrap();
        assert_eq!(list_after.raw_len(), 0);
    }

    #[test]
    fn test_hooks_list_returns_registered_hooks() {
        let lua = lua_with_hooks();

        let hook_fn = lua.create_function(|_, ()| Ok(())).unwrap();
        let register_fn: Function = crap_hooks(&lua).get("register").unwrap();
        let _: () = register_fn.call(("before_change", hook_fn)).unwrap();

        let list: Table = lua
            .load("return crap.hooks.list('before_change')")
            .eval()
            .unwrap();
        assert_eq!(list.raw_len(), 1);
    }

    #[test]
    fn test_hooks_list_empty_event_returns_empty_table() {
        let lua = lua_with_hooks();
        let list: Table = lua
            .load("return crap.hooks.list('nonexistent')")
            .eval()
            .unwrap();
        assert_eq!(list.raw_len(), 0);
    }

    #[test]
    fn test_is_known_event() {
        assert!(is_known_event("before_validate"));
        assert!(is_known_event("before_change"));
        assert!(is_known_event("after_change"));
        assert!(is_known_event("before_read"));
        assert!(is_known_event("after_read"));
        assert!(is_known_event("before_delete"));
        assert!(is_known_event("after_delete"));
        assert!(is_known_event("before_broadcast"));
        assert!(is_known_event("before_render"));
        assert!(!is_known_event("nonexistent"));
        assert!(!is_known_event("on_change"));
        assert!(!is_known_event(""));
    }

    #[test]
    fn test_register_unknown_event_is_hard_error() {
        // Registering an unrecognized event name is rejected — a typo'd event
        // (e.g. "on_change") would otherwise create a dead hook that never fires.
        let lua = lua_with_hooks();
        let register_fn: Function = crap_hooks(&lua).get("register").unwrap();

        let hook_fn = lua.create_function(|_, ()| Ok(())).unwrap();
        let result: LuaResult<()> = register_fn.call(("on_change", hook_fn));
        let err = result.expect_err("unknown event must be rejected");
        assert!(
            err.to_string().contains("unknown event 'on_change'"),
            "expected unknown-event error, got: {err}"
        );

        // Nothing was registered under the bogus name.
        let event_hooks: Table = lua.named_registry_value("_crap_event_hooks").unwrap();
        assert!(
            event_hooks.get::<Value>("on_change").unwrap().is_nil(),
            "no hook list should be created for an unknown event"
        );
    }
}
