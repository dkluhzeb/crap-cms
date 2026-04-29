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
use mlua::{Function, Lua, Table, Value};

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

fn register_template_data_fn(lua: &Lua, name: &str, func: Function) -> mlua::Result<()> {
    let table: Table = lua.named_registry_value(TEMPLATE_DATA_KEY)?;
    table.set(name, func)
}

fn list_template_data(lua: &Lua) -> mlua::Result<Table> {
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

    #[test]
    fn register_and_call_a_template_data_fn() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_template_data(&lua, &crap).unwrap();
        lua.globals().set("crap", crap).unwrap();

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
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_template_data(&lua, &crap).unwrap();
        lua.globals().set("crap", crap).unwrap();

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
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_template_data(&lua, &crap).unwrap();
        lua.globals().set("crap", crap).unwrap();

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
}
