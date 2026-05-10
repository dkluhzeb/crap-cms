//! Register `crap.hooks` — register/remove global event hooks, plus `_crap_event_hooks` storage.

use anyhow::Result;
use mlua::{Function, Lua, Table, Value};
use tracing::warn;

/// Known lifecycle event names. Used to warn on unrecognized event registrations.
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

/// Register `crap.hooks.register`, `crap.hooks.remove`, and `crap.hooks.list`.
pub(super) fn register_hooks(lua: &Lua, crap: &Table) -> Result<()> {
    lua.set_named_registry_value("_crap_event_hooks", lua.create_table()?)?;

    let t = lua.create_table()?;

    t.set(
        "register",
        lua.create_function(|lua, (event, func): (String, Function)| {
            register_hook(lua, &event, func)
        })?,
    )?;
    t.set(
        "remove",
        lua.create_function(|lua, (event, func): (String, Function)| {
            remove_hook(lua, &event, &func)
        })?,
    )?;
    t.set(
        "list",
        lua.create_function(|lua, event: String| list_hooks(lua, &event))?,
    )?;

    crap.set("hooks", t)?;

    Ok(())
}

/// Get the event hook list for an event, creating it if absent.
fn get_or_create_hook_list(lua: &Lua, event: &str) -> mlua::Result<Table> {
    let event_hooks: Table = lua.named_registry_value("_crap_event_hooks")?;

    match event_hooks.get::<Value>(event)? {
        Value::Table(t) => Ok(t),
        _ => {
            let t = lua.create_table()?;

            event_hooks.set(event, t.clone())?;

            Ok(t)
        }
    }
}

/// Register a hook function for an event.
fn register_hook(lua: &Lua, event: &str, func: Function) -> mlua::Result<()> {
    if !is_known_event(event) {
        warn!(
            "crap.hooks.register: unknown event '{event}'. Known events: {}",
            KNOWN_EVENTS.join(", ")
        );
    }

    let list = get_or_create_hook_list(lua, event)?;
    list.set(list.raw_len() + 1, func)
}

/// Remove a hook function from an event by identity (rawequal).
fn remove_hook(lua: &Lua, event: &str, func: &Function) -> mlua::Result<()> {
    let event_hooks: Table = lua.named_registry_value("_crap_event_hooks")?;
    let Value::Table(list) = event_hooks.get::<Value>(event)? else {
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

/// List all hook functions registered for an event.
fn list_hooks(lua: &Lua, event: &str) -> mlua::Result<Table> {
    let event_hooks: Table = lua.named_registry_value("_crap_event_hooks")?;

    match event_hooks.get::<Value>(event)? {
        Value::Table(t) => Ok(t),
        _ => lua.create_table(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{Function, Lua, Table};

    #[test]
    fn test_register_hooks_remove_nonexistent_event_is_noop() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_hooks(&lua, &crap).unwrap();
        let hooks: Table = crap.get("hooks").unwrap();
        let remove_fn: Function = hooks.get("remove").unwrap();
        let dummy_fn = lua.create_function(|_, ()| Ok(())).unwrap();
        let result: mlua::Result<()> = remove_fn.call(("before_change", dummy_fn));
        assert!(
            result.is_ok(),
            "remove on nonexistent event should be a noop"
        );
    }

    #[test]
    fn test_register_hooks_register_and_remove() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_hooks(&lua, &crap).unwrap();
        let hooks: Table = crap.get("hooks").unwrap();
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
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_hooks(&lua, &crap).unwrap();
        lua.globals().set("crap", crap).unwrap();

        let hook_fn = lua.create_function(|_, ()| Ok(())).unwrap();
        let hooks: Table = lua
            .globals()
            .get::<Table>("crap")
            .unwrap()
            .get("hooks")
            .unwrap();
        let register_fn: Function = hooks.get("register").unwrap();
        let _: () = register_fn.call(("before_change", hook_fn)).unwrap();

        let list: Table = lua
            .load("return crap.hooks.list('before_change')")
            .eval()
            .unwrap();
        assert_eq!(list.raw_len(), 1);
    }

    #[test]
    fn test_hooks_list_empty_event_returns_empty_table() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_hooks(&lua, &crap).unwrap();
        lua.globals().set("crap", crap).unwrap();

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
    fn test_register_unknown_event_still_succeeds() {
        // Unknown events log a warning but still register (no hard failure)
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_hooks(&lua, &crap).unwrap();
        let hooks: Table = crap.get("hooks").unwrap();
        let register_fn: Function = hooks.get("register").unwrap();

        let hook_fn = lua.create_function(|_, ()| Ok(())).unwrap();
        let result: mlua::Result<()> = register_fn.call(("unknown_event", hook_fn));
        assert!(result.is_ok(), "unknown events should still register");

        let event_hooks: Table = lua.named_registry_value("_crap_event_hooks").unwrap();
        let list: Table = event_hooks.get("unknown_event").unwrap();
        assert_eq!(list.raw_len(), 1);
    }
}
