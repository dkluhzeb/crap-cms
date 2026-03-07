//! `crap.config` and `crap.locale` namespaces — read-only config access.

use anyhow::Result;
use mlua::{Lua, Table, Value};

use crate::config::CrapConfig;

use super::json_to_lua;

/// Register `crap.config` — read-only config access with dot notation.
pub(super) fn register_config(lua: &Lua, crap: &Table, config: &CrapConfig) -> Result<()> {
    let config_table = lua.create_table()?;
    let config_json = serde_json::to_value(config)
        .map_err(|e| anyhow::anyhow!("Failed to serialize config: {}", e))?;
    let config_lua = json_to_lua(lua, &config_json)?;
    lua.globals().set("_crap_config", config_lua)?;

    let config_get_fn = lua.create_function(|lua, key: String| -> mlua::Result<Value> {
        let config_val: Value = lua.globals().get("_crap_config")?;
        let mut current = config_val;
        for part in key.split('.') {
            match current {
                Value::Table(tbl) => {
                    current = tbl.get(part)?;
                }
                _ => return Ok(Value::Nil),
            }
        }
        Ok(current)
    })?;
    config_table.set("get", config_get_fn)?;
    crap.set("config", config_table)?;
    Ok(())
}

/// Register `crap.locale` — locale configuration access.
pub(super) fn register_locale(lua: &Lua, crap: &Table, config: &CrapConfig) -> Result<()> {
    let locale_table = lua.create_table()?;

    let default_locale = config.locale.default_locale.clone();
    let get_default_fn = lua.create_function(move |_, ()| -> mlua::Result<String> {
        Ok(default_locale.clone())
    })?;
    locale_table.set("get_default", get_default_fn)?;

    let locales = config.locale.locales.clone();
    let get_all_fn = lua.create_function(move |lua, ()| -> mlua::Result<Table> {
        let tbl = lua.create_table()?;
        for (i, l) in locales.iter().enumerate() {
            tbl.set(i + 1, l.as_str())?;
        }
        Ok(tbl)
    })?;
    locale_table.set("get_all", get_all_fn)?;

    let enabled = config.locale.is_enabled();
    let is_enabled_fn = lua.create_function(move |_, ()| -> mlua::Result<bool> {
        Ok(enabled)
    })?;
    locale_table.set("is_enabled", is_enabled_fn)?;

    crap.set("locale", locale_table)?;
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
        let result: mlua::Value = lua
            .load("return crap.config.get('server')")
            .eval()
            .unwrap();

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

        let result: String = lua
            .load("return crap.locale.get_default()")
            .eval()
            .unwrap();

        assert_eq!(result, "fr");
    }

    #[test]
    fn locale_get_default_uses_config_default_en() {
        // Default LocaleConfig has default_locale = "en".
        let config = CrapConfig::default();
        let (lua, _crap) = setup_lua(&config);

        let result: String = lua
            .load("return crap.locale.get_default()")
            .eval()
            .unwrap();

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

        let result: mlua::Table = lua
            .load("return crap.locale.get_all()")
            .eval()
            .unwrap();

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

        let result: mlua::Table = lua
            .load("return crap.locale.get_all()")
            .eval()
            .unwrap();

        let locales: Vec<String> = result
            .sequence_values::<String>()
            .collect::<mlua::Result<_>>()
            .unwrap();

        assert!(locales.is_empty(), "expected empty table when no locales configured");
    }

    // --- crap.locale.is_enabled ---

    #[test]
    fn locale_is_enabled_returns_false_when_no_locales() {
        let config = CrapConfig::default(); // locales = []
        let (lua, _crap) = setup_lua(&config);

        let result: bool = lua
            .load("return crap.locale.is_enabled()")
            .eval()
            .unwrap();

        assert!(!result, "expected false when no locales are configured");
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

        let result: bool = lua
            .load("return crap.locale.is_enabled()")
            .eval()
            .unwrap();

        assert!(result, "expected true when locales are configured");
    }
}
