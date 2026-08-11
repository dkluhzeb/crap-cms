//! Register `crap.routes` — declare custom HTTP endpoints from Lua. The handler
//! lives at `<config_dir>/routes/<name>.lua` and returns
//! `function(ctx) ... return { ... } end`; this API mounts it on the admin
//! server and records its access / rate-limit / csrf metadata.
//!
//! ## Usage
//!
//! ```lua
//! crap.routes.register({
//!   path    = "/webhooks/stripe",      -- literal path (Axum {param} syntax)
//!   method  = "POST",                   -- or { "GET", "POST" }
//!   handler = "routes.stripe_webhook",  -- ref → routes/stripe_webhook.lua
//!   access  = false,                    -- optional: omitted=public, false=disabled, "ref"=gated
//!   rate_limit = { max = 60, window = 60 }, -- optional
//!   csrf    = false,                    -- optional (default false)
//!   options = { ... },                  -- optional, surfaced as ctx.options
//! })
//! ```
//!
//! Recognized keys: `path`, `method`, `handler`, `access`, `rate_limit`,
//! `csrf`, `options`. `path`, `method`, and `handler` are required. Registration
//! is rejected outside the init phase (routes are read once at startup), and a
//! bad method / path / handler ref fails loudly at load.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, LuaSerdeExt as _, Result as LuaResult, Table, Value};

use crate::{
    admin::custom_routes::{ALLOWED_METHODS, normalize_method, validate_path},
    core::HookRef,
    hooks::{lifecycle::InitPhase, lua_api::parse::deny_unknown_keys},
    typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Named registry value holding the array of registered route entry tables.
pub(crate) const ROUTES_KEY: &str = "_crap_routes";

/// Read and validate the `method` field into a list of upper-cased methods.
fn parse_methods(def: &Table) -> LuaResult<Vec<String>> {
    let raw: Value = def.get("method")?;
    let mut methods = Vec::new();

    match raw {
        Value::String(s) => methods.push(normalize_one(&s.to_str()?)?),
        Value::Table(t) => {
            for pair in t.sequence_values::<String>() {
                methods.push(normalize_one(&pair?)?);
            }
        }
        Value::Nil => {
            return Err(RuntimeError(
                "crap.routes.register: `method` is required (a string or array of strings)".into(),
            ));
        }
        other => {
            return Err(RuntimeError(format!(
                "crap.routes.register: `method` must be a string or array, got {}",
                other.type_name()
            )));
        }
    }

    if methods.is_empty() {
        return Err(RuntimeError(
            "crap.routes.register: `method` must name at least one HTTP method".into(),
        ));
    }

    Ok(methods)
}

fn normalize_one(method: &str) -> LuaResult<String> {
    normalize_method(method).map_err(|bad| {
        RuntimeError(format!(
            "crap.routes.register: unsupported method {bad:?} (allowed: {})",
            ALLOWED_METHODS.join(", ")
        ))
    })
}

/// Validate that a value decodes as a `HookRef` (bare string or `{ ref, options }`),
/// returning the original value for verbatim storage in the registry.
fn validate_hook_ref(lua: &Lua, value: Value, field: &str) -> LuaResult<Value> {
    if matches!(value, Value::Nil) {
        return Err(RuntimeError(format!(
            "crap.routes.register: `{field}` is required (a hook ref string or {{ ref, options }})"
        )));
    }
    lua.from_value::<HookRef>(value.clone())
        .map_err(|e| RuntimeError(format!("crap.routes.register: invalid `{field}` ref: {e}")))?;
    Ok(value)
}

/// Register a custom HTTP route. Init-phase only — runtime registration has no
/// effect (routes are mounted once at startup).
#[lua_fn(path = "crap.routes.register")]
fn route_register(
    lua: &Lua,
    #[lua(ty = "table", doc = "Route definition (path, method, handler, …).")] def: Table,
) -> LuaResult<()> {
    if lua.app_data_ref::<InitPhase>().is_none() {
        return Err(RuntimeError(
            "crap.routes.register must be called from init.lua or a definition file \
             — runtime registration has no effect on the mounted routes"
                .into(),
        ));
    }

    deny_unknown_keys(
        &def,
        "crap.routes.register",
        &[
            "path",
            "method",
            "handler",
            "access",
            "rate_limit",
            "csrf",
            "max_body",
            "options",
        ],
    )
    .map_err(|e| RuntimeError(e.to_string()))?;

    let path: String = def.get("path").map_err(|_| {
        RuntimeError("crap.routes.register: `path` is required (a string)".to_string())
    })?;
    validate_path(&path).map_err(|e| RuntimeError(format!("crap.routes.register: {e}")))?;

    let methods = parse_methods(&def)?;
    let handler = validate_hook_ref(lua, def.get("handler")?, "handler")?;

    // Build the normalized entry table stored in the registry.
    let entry = lua.create_table()?;
    entry.set("path", path)?;

    let methods_tbl = lua.create_table()?;
    for (i, m) in (1..).zip(&methods) {
        methods_tbl.set(i, m.as_str())?;
    }
    entry.set("methods", methods_tbl)?;
    entry.set("handler", handler)?;

    // access: nil → public (omit); false → disabled; ref → gated.
    match def.get::<Value>("access")? {
        Value::Nil | Value::Boolean(true) => {}
        Value::Boolean(false) => entry.set("access", false)?,
        other => entry.set("access", validate_hook_ref(lua, other, "access")?)?,
    }

    if let Value::Table(rl) = def.get::<Value>("rate_limit")? {
        deny_unknown_keys(&rl, "crap.routes.register rate_limit", &["max", "window"])
            .map_err(|e| RuntimeError(e.to_string()))?;
        let max: u32 = rl.get("max").map_err(|_| {
            RuntimeError("crap.routes.register: rate_limit.max must be a positive integer".into())
        })?;
        let window: u64 = rl.get("window").map_err(|_| {
            RuntimeError(
                "crap.routes.register: rate_limit.window must be a positive integer".into(),
            )
        })?;
        if max == 0 || window == 0 {
            return Err(RuntimeError(
                "crap.routes.register: rate_limit.max and rate_limit.window must be > 0".into(),
            ));
        }
        entry.set("rate_limit", rl)?;
    }

    // `csrf` is a security opt-in — a present-but-wrong-typed value must fail
    // loudly, never silently fall back to CSRF-disabled (fail-open).
    match def.get::<Value>("csrf")? {
        Value::Nil => {}
        Value::Boolean(csrf) => entry.set("csrf", csrf)?,
        other => {
            return Err(RuntimeError(format!(
                "crap.routes.register: `csrf` must be a boolean, got {}",
                other.type_name()
            )));
        }
    }

    // Accept an integer or a whole-valued number (Lua `2^16` is a float), but
    // reject any other type instead of silently ignoring the cap. The numeric
    // branches re-read as `i64` so mlua does the integral coercion (a
    // fractional float then errors), avoiding a lossy `f64 as i64`.
    match def.get::<Value>("max_body")? {
        Value::Nil => {}
        Value::Integer(_) | Value::Number(_) => {
            let max_body: i64 = def.get("max_body").map_err(|_| {
                RuntimeError(
                    "crap.routes.register: max_body must be a positive integer (bytes)".into(),
                )
            })?;
            if max_body <= 0 {
                return Err(RuntimeError(
                    "crap.routes.register: max_body must be a positive integer (bytes)".into(),
                ));
            }
            entry.set("max_body", max_body)?;
        }
        other => {
            return Err(RuntimeError(format!(
                "crap.routes.register: `max_body` must be a positive integer (bytes), got {}",
                other.type_name()
            )));
        }
    }

    if let Value::Table(opts) = def.get::<Value>("options")? {
        entry.set("options", opts)?;
    }

    let routes: Table = lua.named_registry_value(ROUTES_KEY)?;
    routes.set(routes.raw_len() + 1, entry)?;
    Ok(())
}

/// List every registered route (in declaration order) as a table
/// `{ path = string, methods = string[], access = "public"|"disabled"|"gated" }`.
#[lua_fn(path = "crap.routes.list", returns = "crap.RouteInfo[]")]
fn route_list(lua: &Lua) -> LuaResult<Table> {
    let routes: Table = lua.named_registry_value(ROUTES_KEY)?;
    let out = lua.create_table()?;

    for (i, pair) in (1..).zip(routes.sequence_values::<Table>()) {
        let entry = pair?;
        let info = lua.create_table()?;
        info.set("path", entry.get::<String>("path")?)?;
        info.set("methods", entry.get::<Table>("methods")?)?;
        info.set("access", access_label(&entry))?;
        out.set(i, info)?;
    }

    Ok(out)
}

/// Map a registry entry's `access` field to a stance label for `routes.list`.
fn access_label(entry: &Table) -> &'static str {
    match entry.get::<Value>("access") {
        Ok(Value::Boolean(false)) => "disabled",
        // A ref (string or `{ ref, options }`) is a gate; nil/true is public.
        Ok(Value::String(_) | Value::Table(_)) => "gated",
        _ => "public",
    }
}

lua_table! {
    name: crap_routes,
    path: "crap.routes",
    state: (),
    header: "Declare custom HTTP endpoints. The handler lives at\n`<config_dir>/routes/<name>.lua` and returns `function(ctx) ... end`;\nthis API mounts it on the admin server with its access/rate-limit metadata.",
    fns: [route_register, route_list],
}

/// Register `crap.routes.register` and the storage table.
pub(super) fn register_routes(lua: &Lua) -> Result<()> {
    lua.set_named_registry_value(ROUTES_KEY, lua.create_table()?)?;
    register_crap_routes(lua, ())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lua_in_init_phase() -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_routes(&lua).unwrap();
        lua.set_app_data(InitPhase);
        lua
    }

    fn entries(lua: &Lua) -> Table {
        lua.named_registry_value(ROUTES_KEY).unwrap()
    }

    #[test]
    fn registers_a_route() {
        let lua = lua_in_init_phase();
        lua.load(
            r#"crap.routes.register({ path = "/webhooks/x", method = "POST", handler = "routes.x" })"#,
        )
        .exec()
        .unwrap();

        let routes = entries(&lua);
        assert_eq!(routes.raw_len(), 1);
        let e: Table = routes.get(1).unwrap();
        assert_eq!(e.get::<String>("path").unwrap(), "/webhooks/x");
        let methods: Table = e.get("methods").unwrap();
        assert_eq!(methods.get::<String>(1).unwrap(), "POST");
        assert_eq!(e.get::<String>("handler").unwrap(), "routes.x");
    }

    #[test]
    fn normalizes_and_accepts_method_array() {
        let lua = lua_in_init_phase();
        lua.load(
            r#"crap.routes.register({ path = "/x", method = { "get", "post" }, handler = "routes.x" })"#,
        )
        .exec()
        .unwrap();
        let e: Table = entries(&lua).get(1).unwrap();
        let methods: Table = e.get("methods").unwrap();
        assert_eq!(methods.get::<String>(1).unwrap(), "GET");
        assert_eq!(methods.get::<String>(2).unwrap(), "POST");
    }

    #[test]
    fn rejects_unknown_method() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(
                r#"crap.routes.register({ path = "/x", method = "TRACE", handler = "routes.x" })"#,
            )
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("TRACE"), "{err}");
    }

    #[test]
    fn rejects_reserved_path() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(r#"crap.routes.register({ path = "/admin/x", method = "GET", handler = "routes.x" })"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("reserved"), "{err}");
    }

    #[test]
    fn rejects_missing_handler() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(r#"crap.routes.register({ path = "/x", method = "GET" })"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("handler"), "{err}");
    }

    #[test]
    fn rejects_unknown_key() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(r#"crap.routes.register({ path = "/x", method = "GET", handler = "routes.x", pth = "/y" })"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("pth"), "{err}");
    }

    #[test]
    fn stores_access_false_and_gated_ref() {
        let lua = lua_in_init_phase();
        lua.load(
            r#"
            crap.routes.register({ path = "/pub", method = "GET", handler = "routes.pub" })
            crap.routes.register({ path = "/off", method = "GET", handler = "routes.off", access = false })
            crap.routes.register({ path = "/gated", method = "GET", handler = "routes.g", access = "access.admin_only" })
        "#,
        )
        .exec()
        .unwrap();
        let routes = entries(&lua);
        // public: no `access` key
        let pub_e: Table = routes.get(1).unwrap();
        assert!(matches!(pub_e.get::<Value>("access").unwrap(), Value::Nil));
        // disabled: access == false
        let off_e: Table = routes.get(2).unwrap();
        assert!(matches!(
            off_e.get::<Value>("access").unwrap(),
            Value::Boolean(false)
        ));
        // gated: access == ref string
        let gated_e: Table = routes.get(3).unwrap();
        assert_eq!(
            gated_e.get::<String>("access").unwrap(),
            "access.admin_only"
        );
    }

    #[test]
    fn stores_max_body() {
        let lua = lua_in_init_phase();
        lua.load(
            r#"crap.routes.register({ path = "/u", method = "POST", handler = "routes.u", max_body = 65536 })"#,
        )
        .exec()
        .unwrap();
        let e: Table = entries(&lua).get(1).unwrap();
        assert_eq!(e.get::<i64>("max_body").unwrap(), 65536);
    }

    #[test]
    fn rejects_zero_max_body() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(
                r#"crap.routes.register({ path = "/u", method = "POST", handler = "routes.u", max_body = 0 })"#,
            )
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("max_body"), "{err}");
    }

    #[test]
    fn stores_csrf_boolean() {
        let lua = lua_in_init_phase();
        lua.load(
            r#"crap.routes.register({ path = "/f", method = "POST", handler = "routes.f", csrf = true })"#,
        )
        .exec()
        .unwrap();
        let e: Table = entries(&lua).get(1).unwrap();
        assert!(e.get::<bool>("csrf").unwrap());
    }

    /// `csrf` is a security opt-in: a wrong-typed value must error, not silently
    /// leave CSRF protection disabled (fail-open regression).
    #[test]
    fn rejects_wrong_typed_csrf() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(
                r#"crap.routes.register({ path = "/f", method = "POST", handler = "routes.f", csrf = 1 })"#,
            )
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("csrf") && err.contains("boolean"), "{err}");
    }

    /// A whole-valued float (`2^16` is a float in Lua 5.4) is a valid byte cap,
    /// not silently ignored.
    #[test]
    fn accepts_whole_valued_float_max_body() {
        let lua = lua_in_init_phase();
        lua.load(
            r#"crap.routes.register({ path = "/u", method = "POST", handler = "routes.u", max_body = 2^16 })"#,
        )
        .exec()
        .unwrap();
        let e: Table = entries(&lua).get(1).unwrap();
        assert_eq!(e.get::<i64>("max_body").unwrap(), 65536);
    }

    #[test]
    fn rejects_wrong_typed_max_body() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(
                r#"crap.routes.register({ path = "/u", method = "POST", handler = "routes.u", max_body = "big" })"#,
            )
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("max_body"), "{err}");
    }

    #[test]
    fn rejects_zero_rate_limit() {
        let lua = lua_in_init_phase();
        let err = lua
            .load(
                r#"crap.routes.register({ path = "/x", method = "GET", handler = "routes.x", rate_limit = { max = 0, window = 60 } })"#,
            )
            .exec()
            .unwrap_err()
            .to_string();
        assert!(err.contains("> 0"), "{err}");
    }

    #[test]
    fn list_returns_route_info_tables() {
        let lua = lua_in_init_phase();
        lua.load(
            r#"
            crap.routes.register({ path = "/a", method = "GET", handler = "routes.a" })
            crap.routes.register({ path = "/b", method = { "GET", "POST" }, handler = "routes.b", access = false })
            crap.routes.register({ path = "/c", method = "GET", handler = "routes.c", access = "access.admin" })
        "#,
        )
        .exec()
        .unwrap();
        let list: Table = lua.load("return crap.routes.list()").eval().unwrap();

        let a: Table = list.get(1).unwrap();
        assert_eq!(a.get::<String>("path").unwrap(), "/a");
        assert_eq!(
            a.get::<Table>("methods").unwrap().get::<String>(1).unwrap(),
            "GET"
        );
        assert_eq!(a.get::<String>("access").unwrap(), "public");

        let b: Table = list.get(2).unwrap();
        let b_methods: Table = b.get("methods").unwrap();
        assert_eq!(b_methods.get::<String>(2).unwrap(), "POST");
        assert_eq!(b.get::<String>("access").unwrap(), "disabled");

        let c: Table = list.get(3).unwrap();
        assert_eq!(c.get::<String>("access").unwrap(), "gated");
    }

    #[test]
    fn register_outside_init_phase_is_rejected() {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_routes(&lua).unwrap();
        // no InitPhase
        let err = lua
            .load(r#"crap.routes.register({ path = "/x", method = "GET", handler = "routes.x" })"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("init.lua") || err.contains("runtime registration"),
            "{err}"
        );
    }
}
