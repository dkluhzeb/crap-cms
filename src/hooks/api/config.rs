//! `crap.config` and `crap.locale` namespaces — read-only config access.

use anyhow::{Result, anyhow};
use mlua::{Lua, Result as LuaResult, Table, Value};

use crate::{config::CrapConfig, hooks::api::json_to_lua};

/// Register `crap.config` — read-only config access with dot notation.
pub(super) fn register_config(lua: &Lua, crap: &Table, config: &CrapConfig) -> Result<()> {
    let config_json =
        serde_json::to_value(config).map_err(|e| anyhow!("Failed to serialize config: {e}"))?;
    lua.set_named_registry_value("_crap_config", json_to_lua(lua, &config_json)?)?;

    let t = lua.create_table()?;
    t.set(
        "get",
        lua.create_function(|lua, key: String| config_get(lua, &key))?,
    )?;
    crap.set("config", t)?;

    Ok(())
}

/// Register `crap.locale` — locale configuration access.
pub(super) fn register_locale(lua: &Lua, crap: &Table, config: &CrapConfig) -> Result<()> {
    let t = lua.create_table()?;

    let default = config.locale.default_locale.clone();
    t.set(
        "get_default",
        lua.create_function(move |_, ()| Ok(default.clone()))?,
    )?;

    let locales = config.locale.locales.clone();
    t.set(
        "get_all",
        lua.create_function(move |lua, ()| locale_get_all(lua, &locales))?,
    )?;

    let enabled = config.locale.is_enabled();
    t.set("is_enabled", lua.create_function(move |_, ()| Ok(enabled))?)?;

    crap.set("locale", t)?;

    Ok(())
}

/// Traverse a dot-separated key path through the config registry value.
fn config_get(lua: &Lua, key: &str) -> LuaResult<Value> {
    let mut current: Value = lua.named_registry_value("_crap_config")?;

    for part in key.split('.') {
        let Value::Table(tbl) = current else {
            return Ok(Value::Nil);
        };
        current = tbl.get(part)?;
    }

    Ok(current)
}

/// Return all configured locales as a Lua sequence table.
fn locale_get_all(lua: &Lua, locales: &[String]) -> LuaResult<Table> {
    let tbl = lua.create_table()?;
    for (i, l) in locales.iter().enumerate() {
        tbl.set(i + 1, l.as_str())?;
    }
    Ok(tbl)
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
                host: host.to_string(),
                ..ServerConfig::default()
            },
            ..CrapConfig::default()
        }
    }

    /// Create a fresh Lua VM, register `crap.config` and `crap.locale` on a `crap`
    /// table, and return the `(lua, crap_table)` pair for use in assertions.
    fn setup_lua(config: &CrapConfig) -> (Lua, mlua::Table) {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_config(&lua, &crap, config).unwrap();
        register_locale(&lua, &crap, config).unwrap();
        lua.globals().set("crap", crap.clone()).unwrap();
        (lua, crap)
    }

    // --- crap.config.get ---

    #[test]
    fn config_get_nested_key_returns_value() {
        let config = make_config_with_host("127.0.0.1");
        let (lua, _crap) = setup_lua(&config);

        let result: String = lua
            .load("return crap.config.get('server.host')")
            .eval()
            .unwrap();

        assert_eq!(result, "127.0.0.1");
    }

    #[test]
    fn config_get_top_level_key_returns_table() {
        let config = CrapConfig::default();
        let (lua, _crap) = setup_lua(&config);

        // "server" without a sub-key should return a table, not nil.
        let result: mlua::Value = lua.load("return crap.config.get('server')").eval().unwrap();

        assert!(
            matches!(result, mlua::Value::Table(_)),
            "expected a table for top-level key"
        );
    }

    #[test]
    fn config_get_nonexistent_top_level_key_returns_nil() {
        let config = CrapConfig::default();
        let (lua, _crap) = setup_lua(&config);

        let result: mlua::Value = lua
            .load("return crap.config.get('nonexistent')")
            .eval()
            .unwrap();

        assert!(
            matches!(result, mlua::Value::Nil),
            "expected nil for missing top-level key"
        );
    }

    #[test]
    fn config_get_nonexistent_nested_key_returns_nil() {
        let config = CrapConfig::default();
        let (lua, _crap) = setup_lua(&config);

        let result: mlua::Value = lua
            .load("return crap.config.get('nonexistent.key')")
            .eval()
            .unwrap();

        assert!(
            matches!(result, mlua::Value::Nil),
            "expected nil for missing nested key"
        );
    }

    #[test]
    fn config_get_deeply_nested_missing_key_returns_nil() {
        // Traversal hits a non-table (e.g. a string) before exhausting parts.
        let config = make_config_with_host("localhost");
        let (lua, _crap) = setup_lua(&config);

        // server.host is a string; going deeper should return nil, not panic.
        let result: mlua::Value = lua
            .load("return crap.config.get('server.host.subkey')")
            .eval()
            .unwrap();

        assert!(
            matches!(result, mlua::Value::Nil),
            "expected nil when traversal hits a non-table value"
        );
    }

    #[test]
    fn config_get_integer_value() {
        let config = CrapConfig::default();
        let (lua, _crap) = setup_lua(&config);

        // server.admin_port defaults to 3000.
        let result: i64 = lua
            .load("return crap.config.get('server.admin_port')")
            .eval()
            .unwrap();

        assert_eq!(result, 3000);
    }

    // --- crap.locale.get_default ---

    #[test]
    fn locale_get_default_returns_default_locale() {
        let config = CrapConfig {
            locale: LocaleConfig {
                default_locale: "fr".to_string(),
                locales: vec!["fr".to_string(), "de".to_string()],
                ..LocaleConfig::default()
            },
            ..CrapConfig::default()
        };
        let (lua, _crap) = setup_lua(&config);

        let result: String = lua.load("return crap.locale.get_default()").eval().unwrap();

        assert_eq!(result, "fr");
    }

    #[test]
    fn locale_get_default_uses_config_default_en() {
        // Default LocaleConfig has default_locale = "en".
        let config = CrapConfig::default();
        let (lua, _crap) = setup_lua(&config);

        let result: String = lua.load("return crap.locale.get_default()").eval().unwrap();

        assert_eq!(result, "en");
    }

    // --- crap.locale.get_all ---

    #[test]
    fn locale_get_all_returns_configured_locales() {
        let config = CrapConfig {
            locale: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string(), "fr".to_string()],
                ..LocaleConfig::default()
            },
            ..CrapConfig::default()
        };
        let (lua, _crap) = setup_lua(&config);

        let result: mlua::Table = lua.load("return crap.locale.get_all()").eval().unwrap();

        let locales: Vec<String> = result
            .sequence_values::<String>()
            .collect::<mlua::Result<_>>()
            .unwrap();

        assert_eq!(locales, vec!["en", "de", "fr"]);
    }

    #[test]
    fn locale_get_all_returns_empty_table_when_no_locales() {
        // Default LocaleConfig has no locales (disabled).
        let config = CrapConfig::default();
        let (lua, _crap) = setup_lua(&config);

        let result: mlua::Table = lua.load("return crap.locale.get_all()").eval().unwrap();

        let locales: Vec<String> = result
            .sequence_values::<String>()
            .collect::<mlua::Result<_>>()
            .unwrap();

        assert!(
            locales.is_empty(),
            "expected empty table when no locales configured"
        );
    }

    // --- crap.locale.is_enabled ---

    #[test]
    fn locale_is_enabled_returns_false_when_no_locales() {
        let config = CrapConfig::default(); // locales = []
        let (lua, _crap) = setup_lua(&config);

        let result: bool = lua.load("return crap.locale.is_enabled()").eval().unwrap();

        assert!(!result, "expected false when no locales are configured");
    }

    #[test]
    fn config_not_accessible_from_lua_globals() {
        let config = CrapConfig::default();
        let (lua, _crap) = setup_lua(&config);

        let result: mlua::Value = lua.load("return _crap_config").eval().unwrap();

        assert!(
            matches!(result, mlua::Value::Nil),
            "expected _crap_config to be nil in Lua globals (stored in registry)"
        );
    }

    #[test]
    fn locale_is_enabled_returns_true_when_locales_present() {
        let config = CrapConfig {
            locale: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                ..LocaleConfig::default()
            },
            ..CrapConfig::default()
        };
        let (lua, _crap) = setup_lua(&config);

        let result: bool = lua.load("return crap.locale.is_enabled()").eval().unwrap();

        assert!(result, "expected true when locales are configured");
    }
}
