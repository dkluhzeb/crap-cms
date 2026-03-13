//! Register `crap.hooks` — register/remove global event hooks, plus `_crap_event_hooks` storage.

use anyhow::Result;
use mlua::{Function, Lua, Table, Value};

pub(super) fn register_hooks(lua: &Lua, crap: &Table) -> Result<()> {
    // _crap_event_hooks — Lua-side storage for registered global hooks
    let event_hooks = lua.create_table()?;
    lua.globals().set("_crap_event_hooks", event_hooks)?;

    let hooks_table = lua.create_table()?;

    let register_fn = lua.create_function(|lua, (event, func): (String, Function)| {
        let globals = lua.globals();
        let event_hooks: Table = globals.get("_crap_event_hooks")?;
        let list: Table = match event_hooks.get::<Value>(event.as_str())? {
            Value::Table(t) => t,
            _ => {
                let t = lua.create_table()?;
                event_hooks.set(event.as_str(), t.clone())?;
                t
            }
        };
        let len = list.raw_len();
        list.set(len + 1, func)?;
        Ok(())
    })?;
    hooks_table.set("register", register_fn)?;

    let remove_fn = lua.create_function(|lua, (event, func): (String, Function)| {
        let globals = lua.globals();
        let event_hooks: Table = globals.get("_crap_event_hooks")?;
        let list: Table = match event_hooks.get::<Value>(event.as_str())? {
            Value::Table(t) => t,
            _ => return Ok(()),
        };
        let rawequal: Function = globals.get("rawequal")?;
        let len = list.raw_len();
        let mut remove_idx = None;
        for i in 1..=len {
            let entry: Value = list.raw_get(i)?;
            let eq: bool = rawequal.call((entry, func.clone()))?;

            if eq {
                remove_idx = Some(i);
                break;
            }
        }
        if let Some(idx) = remove_idx {
            let table_remove: Function = lua.load("table.remove").eval()?;
            table_remove.call::<()>((list, idx))?;
        }
        Ok(())
    })?;
    hooks_table.set("remove", remove_fn)?;

    crap.set("hooks", hooks_table)?;
    Ok(())
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

        let event_hooks: Table = lua.globals().get("_crap_event_hooks").unwrap();
        let list: Table = event_hooks.get("after_change").unwrap();
        assert_eq!(list.raw_len(), 1);

        let _: () = remove_fn.call(("after_change", hook_fn)).unwrap();
        let list_after: Table = event_hooks.get("after_change").unwrap();
        assert_eq!(list_after.raw_len(), 0);
    }
}
