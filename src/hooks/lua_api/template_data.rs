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
//! actually calls `{{data "fetch_weather"}}` — and on **every** such lookup:
//! results are not cached, so a template that needs the value twice binds it
//! once with `{{#with (data "…")}}`. Registering the same name twice is an
//! error.

use anyhow::Result;
use mlua::{Error::RuntimeError, Function, Lua, Result as LuaResult, Table, Value};

use super::utils::require_init_phase;
use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Named registry value that holds the `name → Function` map.
pub(crate) const TEMPLATE_DATA_KEY: &str = "_crap_template_data";

/// Register a named template-data function. Must be called from
/// init.lua or a definition file — runtime registration only lands in
/// one VM of the pool and is intermittent across requests.
#[lua_fn(path = "crap.template_data.register")]
fn template_data_register(
    lua: &Lua,
    #[lua(
        doc = "Unique name (used as `{{data \"name\"}}` in templates); registering a name twice is an error."
    )]
    name: String,
    #[lua(
        doc = "Lua function called on each `{{data}}` lookup; returns any JSON-encodable value."
    )]
    func: Function,
) -> LuaResult<()> {
    require_init_phase(
        lua,
        "crap.template_data.register must be called from init.lua or a definition \
         file — runtime registration only lands in one VM of the pool and is \
         intermittent across requests",
    )?;

    let table: Table = lua.named_registry_value(TEMPLATE_DATA_KEY)?;

    if table.contains_key(name.as_str())? {
        return Err(RuntimeError(format!(
            "crap.template_data.register: '{name}' is already registered — each name \
             maps to one function"
        )));
    }

    table.set(name, func)
}

/// List the names of every registered template-data function (in
/// iteration order — not deterministic across runs).
#[lua_fn(path = "crap.template_data.list", returns = "string[]")]
fn template_data_list(lua: &Lua) -> LuaResult<Table> {
    let table: Table = lua.named_registry_value(TEMPLATE_DATA_KEY)?;
    let names = lua.create_table()?;
    for (i, pair) in (1..).zip(table.pairs::<Value, Value>()) {
        let (key, _) = pair?;
        names.set(i, key)?;
    }
    Ok(names)
}

lua_table! {
    name: crap_template_data,
    path: "crap.template_data",
    state: (),
    header: "Register named template-data functions called lazily by the\n`{{data \"name\"}}` Handlebars helper. Each function is invoked on every\n`{{data}}` lookup that names it; results are not cached, so bind a value\nused twice once with `{{#with (data \"name\")}}`.",
    fns: [template_data_register, template_data_list],
}

/// Register `crap.template_data.register` and the storage table.
/// Parent `crap` must already be in globals.
pub(super) fn register_template_data(lua: &Lua) -> Result<()> {
    lua.set_named_registry_value(TEMPLATE_DATA_KEY, lua.create_table()?)?;
    register_crap_template_data(lua, ())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::lifecycle::InitPhase;
    use mlua::Function;

    /// Build a Lua VM with `crap.template_data` registered AND the
    /// `InitPhase` marker set, mimicking the state during init-time loading.
    fn lua_in_init_phase() -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_template_data(&lua).unwrap();
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

    /// Regression: a second registration under the same name silently
    /// replaced the first; `crap.pages.register` already rejected duplicates.
    #[test]
    fn registering_a_name_twice_is_rejected() {
        let lua = lua_in_init_phase();

        let err = lua
            .load(
                r#"
                crap.template_data.register("weather", function() return 1 end)
                crap.template_data.register("weather", function() return 2 end)
            "#,
            )
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("already registered"), "{err}");

        let table: Table = lua.named_registry_value(TEMPLATE_DATA_KEY).unwrap();
        let func: Function = table.get("weather").unwrap();
        assert_eq!(
            func.call::<i64>(()).unwrap(),
            1,
            "the first registration stays"
        );
    }

    /// Regression: `crap.template_data.register` called outside the init
    /// phase must fail loudly. Each VM has its own `template_data` registry,
    /// so a runtime registration would only land in the current VM —
    /// future renders served by other VMs would not see the function,
    /// producing intermittent visibility across requests.
    #[test]
    fn register_outside_init_phase_is_rejected() {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_template_data(&lua).unwrap();
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
