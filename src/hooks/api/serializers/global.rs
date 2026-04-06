//! Lua table serializer for GlobalDefinition.

use mlua::{Lua, Result as LuaResult, Table};

use crate::core::collection::GlobalDefinition;

use super::collection::{
    access_to_lua, collection_hooks_to_lua, fields_to_lua, labels_to_lua, live_to_lua, mcp_to_lua,
};

/// Convert a GlobalDefinition to a full Lua table compatible with parse_global_definition().
pub(crate) fn global_config_to_lua(lua: &Lua, def: &GlobalDefinition) -> LuaResult<Table> {
    let tbl = lua.create_table()?;

    labels_to_lua(lua, &tbl, &def.labels)?;
    fields_to_lua(lua, &tbl, &def.fields)?;

    tbl.set("hooks", collection_hooks_to_lua(lua, &def.hooks)?)?;

    access_to_lua(lua, &tbl, &def.access)?;
    mcp_to_lua(lua, &tbl, &def.mcp)?;
    live_to_lua(&tbl, &def.live)?;

    Ok(tbl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        collection::{Access, GlobalDefinition, LiveSetting, McpConfig},
        field::{FieldDefinition, FieldType, LocalizedString},
    };
    use mlua::Lua;

    use crate::core::collection::Labels;

    #[test]
    fn test_global_config_to_lua_basic() {
        let lua = Lua::new();
        let mut def = GlobalDefinition::new("settings");
        def.labels = Labels {
            singular: Some(LocalizedString::Plain("Settings".to_string())),
            plural: None,
        };
        def.fields = vec![FieldDefinition::builder("site_name", FieldType::Text).build()];
        let tbl = global_config_to_lua(&lua, &def).unwrap();
        let labels: mlua::Table = tbl.get("labels").unwrap();
        let singular: String = labels.get("singular").unwrap();
        assert_eq!(singular, "Settings");
        let fields: mlua::Table = tbl.get("fields").unwrap();
        let f1: mlua::Table = fields.get(1).unwrap();
        assert_eq!(f1.get::<String>("name").unwrap(), "site_name");
    }

    #[test]
    fn test_global_config_to_lua_with_live() {
        let lua = Lua::new();
        let mut def = GlobalDefinition::new("settings");
        def.access = Access {
            read: Some("hooks.access.allow".to_string()),
            ..Default::default()
        };
        def.live = Some(LiveSetting::Disabled);
        let tbl = global_config_to_lua(&lua, &def).unwrap();
        assert!(!tbl.get::<bool>("live").unwrap());
        let access: mlua::Table = tbl.get("access").unwrap();
        assert_eq!(access.get::<String>("read").unwrap(), "hooks.access.allow");
    }

    #[test]
    fn test_global_config_to_lua_mcp_description() {
        let lua = Lua::new();
        let mut def = GlobalDefinition::new("settings");
        def.mcp = McpConfig {
            description: Some("Global site settings".to_string()),
        };
        let tbl = global_config_to_lua(&lua, &def).unwrap();
        let mcp: mlua::Table = tbl.get("mcp").unwrap();
        assert_eq!(
            mcp.get::<String>("description").unwrap(),
            "Global site settings"
        );
    }

    #[test]
    fn test_global_config_to_lua_access_trash() {
        let lua = Lua::new();
        let mut def = GlobalDefinition::new("settings");
        def.access = Access {
            trash: Some("access.editor".to_string()),
            ..Default::default()
        };
        let tbl = global_config_to_lua(&lua, &def).unwrap();
        let access: mlua::Table = tbl.get("access").unwrap();
        assert_eq!(access.get::<String>("trash").unwrap(), "access.editor");
    }

    #[test]
    fn test_global_config_to_lua_live_function() {
        let lua = Lua::new();
        let mut def = GlobalDefinition::new("settings");
        def.live = Some(LiveSetting::Function("hooks.live.settings".to_string()));
        let tbl = global_config_to_lua(&lua, &def).unwrap();
        assert_eq!(tbl.get::<String>("live").unwrap(), "hooks.live.settings");
    }
}
