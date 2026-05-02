//! Register `crap.template_data` — register named template-data functions
//! that the `{{data "name"}}` Handlebars helper can call on demand.
//!
//! Usage from Lua:
//!
//! ```lua
//! crap.template_data.register("fetch_weather", function()
//!   return { temp = 22, condition = "sunny" }
//! end)
//! ```
//!
//! Usage from a template:
//!
//! ```hbs
//! {{#with (data "fetch_weather")}}
//!   <p>{{temp}}°C, {{condition}}</p>
//! {{/with}}
//! ```
//!
//! The function is invoked lazily — only when a rendering template
//! actually calls `{{data "fetch_weather"}}` — and only once per HTTP
//! request thanks to the per-request VM acquisition.

use anyhow::Result;
use mlua::{Error::RuntimeError, Function, Lua, Result as LuaResult, Table, Value};

use crate::hooks::lifecycle::InitPhase;

/// Named registry value that holds the `name → Function` map.
pub(crate) const TEMPLATE_DATA_KEY: &str = "_crap_template_data";

/// Register `crap.template_data.register`, plus the storage table.
pub(super) fn register_template_data(lua: &Lua, crap: &Table) -> Result<()> {
    lua.set_named_registry_value(TEMPLATE_DATA_KEY, lua.create_table()?)?;

    let t = lua.create_table()?;

    t.set(
        "register",
        lua.create_function(|lua, (name, func): (String, Function)| {
            register_template_data_fn(lua, &name, func)
        })?,
    )?;
    t.set(
        "list",
        lua.create_function(|lua, ()| list_template_data(lua))?,
    )?;

    crap.set("template_data", t)?;

    Ok(())
}

fn register_template_data_fn(lua: &Lua, name: &str, func: Function) -> LuaResult<()> {
    // template_data is read per-render from whichever Lua VM the helper
    // acquires from the pool. A runtime registration would only land in
    // the current VM, so renders served by other VMs would not see the
    // function — the result is intermittent visibility. Refuse explicitly.
    if lua.app_data_ref::<InitPhase>().is_none() {
        return Err(RuntimeError(
            "crap.template_data.register must be called from init.lua or a definition \
             file — runtime registration only lands in one VM of the pool and is \
             intermittent across requests"
                .into(),
        ));
    }

    let table: Table = lua.named_registry_value(TEMPLATE_DATA_KEY)?;
    table.set(name, func)
}

fn list_template_data(lua: &Lua) -> LuaResult<Table> {
    let table: Table = lua.named_registry_value(TEMPLATE_DATA_KEY)?;
    let names = lua.create_table()?;
    let mut i = 1;
    for pair in table.pairs::<Value, Value>() {
        let (key, _) = pair?;
        names.set(i, key)?;
        i += 1;
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Function;

    /// Build a Lua VM with `crap.template_data` registered AND the
    /// `InitPhase` marker set, mimicking the state during init-time loading.
    fn lua_in_init_phase() -> Lua {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_template_data(&lua, &crap).unwrap();
        lua.globals().set("crap", crap).unwrap();
        lua.set_app_data(InitPhase);
        lua
    }

    #[test]
    fn register_and_call_a_template_data_fn() {
        let lua = lua_in_init_phase();

        lua.load(
            r#"
            crap.template_data.register("weather", function()
              return { temp = 22, condition = "sunny" }
            end)
        "#,
        )
        .exec()
        .unwrap();

        let table: Table = lua.named_registry_value(TEMPLATE_DATA_KEY).unwrap();
        let func: Function = table.get("weather").unwrap();
        let result: Table = func.call(()).unwrap();
        assert_eq!(result.get::<i64>("temp").unwrap(), 22);
        assert_eq!(result.get::<String>("condition").unwrap(), "sunny");
    }

    #[test]
    fn register_overwrites_existing_name() {
        let lua = lua_in_init_phase();

        lua.load(
            r#"
            crap.template_data.register("x", function() return 1 end)
            crap.template_data.register("x", function() return 2 end)
        "#,
        )
        .exec()
        .unwrap();

        let table: Table = lua.named_registry_value(TEMPLATE_DATA_KEY).unwrap();
        let func: Function = table.get("x").unwrap();
        let result: i64 = func.call(()).unwrap();
        assert_eq!(result, 2);
    }

    #[test]
    fn list_returns_registered_names() {
        let lua = lua_in_init_phase();

        lua.load(
            r#"
            crap.template_data.register("weather", function() return 1 end)
            crap.template_data.register("inbox_count", function() return 2 end)
        "#,
        )
        .exec()
        .unwrap();

        let names: Table = lua.load("return crap.template_data.list()").eval().unwrap();
        assert_eq!(names.raw_len(), 2);
    }

    /// Regression: `crap.template_data.register` called outside the init
    /// phase must fail loudly. Each VM has its own template_data registry,
    /// so a runtime registration would only land in the current VM —
    /// future renders served by other VMs would not see the function,
    /// producing intermittent visibility across requests.
    #[test]
    fn register_outside_init_phase_is_rejected() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_template_data(&lua, &crap).unwrap();
        lua.globals().set("crap", crap).unwrap();
        // No `set_app_data(InitPhase)` — simulating a runtime hook.

        let err = lua
            .load(r#"crap.template_data.register("widget", function() return {} end)"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("init.lua") || err.contains("intermittent"),
            "expected init-only error message, got: {err}"
        );

        let table: Table = lua.named_registry_value(TEMPLATE_DATA_KEY).unwrap();
        let entry: Result<Function, _> = table.get("widget");
        assert!(
            entry.is_err(),
            "callback must NOT be registered when refused"
        );
    }
}
