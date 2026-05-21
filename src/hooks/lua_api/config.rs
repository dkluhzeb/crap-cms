//! `crap.config` and `crap.locale` namespaces — read-only config access.

use anyhow::{Result, anyhow};
use mlua::{Lua, Result as LuaResult, Table, Value};

use crate::config::CrapConfig;
use crate::hooks::lua_api::json_to_lua;
use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

// ── crap.config ──────────────────────────────────────────────────────

/// Get a configuration value using dot notation.
#[lua_fn(
    path = "crap.config.get",
    returns = "any",
    returns_doc = "Value at the key path, or nil if any segment is missing or not a table."
)]
fn config_get(
    lua: &Lua,
    #[lua(doc = "Dot-separated config key (e.g., \"server.admin_port\").")] key: String,
) -> LuaResult<Value> {
    let mut current: Value = lua.named_registry_value("_crap_config")?;

    for part in key.split('.') {
        let Value::Table(tbl) = current else {
            return Ok(Value::Nil);
        };
        current = tbl.get(part)?;
    }

    Ok(current)
}

lua_table! {
    name: crap_config,
    path: "crap.config",
    state: (),
    header: "Read-only access to crap.toml configuration values.\nValues are a snapshot from startup — changes to crap.toml after\nstartup won't be reflected until restart.",
    fns: [config_get],
}

/// Register `crap.config` — read-only config access with dot notation.
/// Parent `crap` must already be in globals.
pub(super) fn register_config(lua: &Lua, config: &CrapConfig) -> Result<()> {
    let config_json =
        serde_json::to_value(config).map_err(|e| anyhow!("Failed to serialize config: {e}"))?;
    lua.set_named_registry_value("_crap_config", json_to_lua(lua, &config_json)?)?;
    register_crap_config(lua, ())?;
    Ok(())
}

// ── crap.locale ──────────────────────────────────────────────────────

/// Snapshot of the locale config fields needed by `crap.locale.*` —
/// the namespace is read-only and the closure state is captured once
/// at registration time.
pub(super) struct LocaleState {
    default: String,
    locales: Vec<String>,
    enabled: bool,
}

/// Get the default locale (e.g., `"en"`).
#[lua_fn(path = "crap.locale.get_default", returns_doc = "Default locale code.")]
fn locale_get_default(state: &LocaleState, _: &Lua) -> LuaResult<String> {
    Ok(state.default.clone())
}

/// List every configured locale (in the order declared in `crap.toml`).
#[lua_fn(
    path = "crap.locale.get_all",
    returns = "string[]",
    returns_doc = "Configured locale codes (insertion order)."
)]
fn locale_get_all(state: &LocaleState, lua: &Lua) -> LuaResult<Table> {
    let tbl = lua.create_table()?;
    for (i, l) in state.locales.iter().enumerate() {
        tbl.set(i + 1, l.as_str())?;
    }
    Ok(tbl)
}

/// Whether localization is enabled (true when at least one locale is
/// configured).
#[lua_fn(
    path = "crap.locale.is_enabled",
    returns_doc = "True iff at least one locale is configured."
)]
fn locale_is_enabled(state: &LocaleState, _: &Lua) -> LuaResult<bool> {
    Ok(state.enabled)
}

lua_table! {
    name: crap_locale,
    path: "crap.locale",
    state: LocaleState,
    header: "Locale configuration access (read-only).\nAvailable in init.lua and hook functions.",
    fns: [locale_get_default, locale_get_all, locale_is_enabled],
}

/// Register `crap.locale` — locale configuration access. Parent `crap`
/// must already be in globals.
pub(super) fn register_locale(lua: &Lua, config: &CrapConfig) -> Result<()> {
    let state = LocaleState {
        default: config.locale.default_locale.clone(),
        locales: config.locale.locales.clone(),
        enabled: config.locale.is_enabled(),
    };
    register_crap_locale(lua, state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LocaleConfig, ServerConfig};
    use mlua::Lua;

    /// Build a minimal `CrapConfig` with a custom server host for testing
    /// `crap.config.get` dot-notation traversal.
    fn make_config_with_host(host: &str) -> CrapConfig {
        CrapConfig {
            server: ServerConfig {
                host: host.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn setup_lua(config: &CrapConfig) -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_config(&lua, config).unwrap();
        register_locale(&lua, config).unwrap();
        lua
    }

    #[test]
    fn config_get_returns_top_level_value() {
        let lua = setup_lua(&make_config_with_host("0.0.0.0"));
        let result: String = lua
            .load(r#"return crap.config.get("server.host")"#)
            .eval()
            .unwrap();
        assert_eq!(result, "0.0.0.0");
    }

    #[test]
    fn config_get_missing_returns_nil() {
        let lua = setup_lua(&CrapConfig::default());
        let result: Value = lua
            .load(r#"return crap.config.get("server.does_not_exist")"#)
            .eval()
            .unwrap();
        assert!(matches!(result, Value::Nil));
    }

    fn cfg_with_locale(default: &str, locales: Vec<String>) -> CrapConfig {
        CrapConfig {
            locale: LocaleConfig {
                default_locale: default.into(),
                locales,
                fallback: true,
            },
            ..Default::default()
        }
    }

    #[test]
    fn locale_get_default_returns_configured_default() {
        let cfg = cfg_with_locale("de", vec!["en".into(), "de".into()]);
        let lua = setup_lua(&cfg);
        let result: String = lua.load("return crap.locale.get_default()").eval().unwrap();
        assert_eq!(result, "de");
    }

    #[test]
    fn locale_get_all_returns_sequence() {
        let cfg = cfg_with_locale("en", vec!["en".into(), "de".into(), "fr".into()]);
        let lua = setup_lua(&cfg);
        let result: Table = lua.load("return crap.locale.get_all()").eval().unwrap();
        assert_eq!(result.raw_len(), 3);
        assert_eq!(result.get::<String>(1).unwrap(), "en");
        assert_eq!(result.get::<String>(3).unwrap(), "fr");
    }

    #[test]
    fn locale_is_enabled_true_with_multiple_locales() {
        let cfg = cfg_with_locale("en", vec!["en".into(), "de".into()]);
        let lua = setup_lua(&cfg);
        let result: bool = lua.load("return crap.locale.is_enabled()").eval().unwrap();
        assert!(result);
    }

    #[test]
    fn locale_is_enabled_false_with_no_locales() {
        let cfg = CrapConfig::default();
        // Default `LocaleConfig` has no locales, so `is_enabled` is false.
        assert!(cfg.locale.locales.is_empty());
        let lua = setup_lua(&cfg);
        let result: bool = lua.load("return crap.locale.is_enabled()").eval().unwrap();
        assert!(!result);
    }
}
