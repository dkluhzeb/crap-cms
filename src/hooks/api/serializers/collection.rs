//! Serializers for CollectionDefinition and GlobalDefinition to Lua tables.

use mlua::{Lua, Result as LuaResult, Table};

use crate::core::{
    CollectionDefinition, FieldDefinition,
    collection::{Access, Hooks, Labels, LiveSetting, McpConfig},
};

use super::{
    auth::collection_auth_to_lua, fields::field_config_to_lua, helpers::localized_string_to_lua,
    upload::collection_upload_to_lua,
};

/// Serialize labels to a Lua table.
pub(super) fn labels_to_lua(lua: &Lua, tbl: &Table, labels: &Labels) -> LuaResult<()> {
    let labels_tbl = lua.create_table()?;

    if let Some(ref s) = labels.singular {
        labels_tbl.set("singular", localized_string_to_lua(lua, s)?)?;
    }

    if let Some(ref s) = labels.plural {
        labels_tbl.set("plural", localized_string_to_lua(lua, s)?)?;
    }

    tbl.set("labels", labels_tbl)
}

/// Serialize access config to a Lua table.
pub(super) fn access_to_lua(lua: &Lua, tbl: &Table, access: &Access) -> LuaResult<()> {
    let access_tbl = lua.create_table()?;

    if let Some(ref s) = access.read {
        access_tbl.set("read", s.as_str())?;
    }

    if let Some(ref s) = access.create {
        access_tbl.set("create", s.as_str())?;
    }

    if let Some(ref s) = access.update {
        access_tbl.set("update", s.as_str())?;
    }

    if let Some(ref s) = access.delete {
        access_tbl.set("delete", s.as_str())?;
    }

    if let Some(ref s) = access.trash {
        access_tbl.set("trash", s.as_str())?;
    }

    tbl.set("access", access_tbl)
}

/// Serialize live setting to a Lua table.
pub(super) fn live_to_lua(tbl: &Table, live: &Option<LiveSetting>) -> LuaResult<()> {
    match live {
        None => tbl.set("live", true),
        Some(LiveSetting::Disabled) => tbl.set("live", false),
        Some(LiveSetting::Function(s)) => tbl.set("live", s.as_str()),
    }
}

/// Serialize MCP config to a Lua table.
pub(super) fn mcp_to_lua(lua: &Lua, tbl: &Table, mcp: &McpConfig) -> LuaResult<()> {
    if let Some(ref desc) = mcp.description {
        let mcp_tbl = lua.create_table()?;
        mcp_tbl.set("description", desc.as_str())?;
        tbl.set("mcp", mcp_tbl)?;
    }

    Ok(())
}

/// Serialize fields to a Lua sequence table.
pub(super) fn fields_to_lua(lua: &Lua, tbl: &Table, fields: &[FieldDefinition]) -> LuaResult<()> {
    let arr = lua.create_table()?;

    for (i, f) in fields.iter().enumerate() {
        arr.set(i + 1, field_config_to_lua(lua, f)?)?;
    }

    tbl.set("fields", arr)
}

/// Convert a CollectionDefinition to a full Lua table compatible with parse_collection_definition().
pub(crate) fn collection_config_to_lua(lua: &Lua, def: &CollectionDefinition) -> LuaResult<Table> {
    let tbl = lua.create_table()?;

    labels_to_lua(lua, &tbl, &def.labels)?;

    tbl.set("timestamps", def.timestamps)?;

    // admin
    let admin = lua.create_table()?;

    if let Some(ref s) = def.admin.use_as_title {
        admin.set("use_as_title", s.as_str())?;
    }

    if let Some(ref s) = def.admin.default_sort {
        admin.set("default_sort", s.as_str())?;
    }

    if def.admin.hidden {
        admin.set("hidden", true)?;
    }

    if !def.admin.list_searchable_fields.is_empty() {
        let lsf = lua.create_table()?;

        for (i, f) in def.admin.list_searchable_fields.iter().enumerate() {
            lsf.set(i + 1, f.as_str())?;
        }

        admin.set("list_searchable_fields", lsf)?;
    }

    tbl.set("admin", admin)?;

    fields_to_lua(lua, &tbl, &def.fields)?;

    tbl.set("hooks", collection_hooks_to_lua(lua, &def.hooks)?)?;

    access_to_lua(lua, &tbl, &def.access)?;
    mcp_to_lua(lua, &tbl, &def.mcp)?;
    collection_auth_to_lua(lua, &tbl, def)?;
    collection_upload_to_lua(lua, &tbl, def)?;
    live_to_lua(&tbl, &def.live)?;

    // soft_delete
    if def.soft_delete {
        tbl.set("soft_delete", true)?;
    }

    if let Some(ref retention) = def.soft_delete_retention {
        tbl.set("soft_delete_retention", retention.as_str())?;
    }

    // versions
    if let Some(ref v) = def.versions {
        if v.drafts && v.max_versions == 0 {
            tbl.set("versions", true)?;
        } else {
            let vt = lua.create_table()?;

            vt.set("drafts", v.drafts)?;

            if v.max_versions > 0 {
                vt.set("max_versions", v.max_versions)?;
            }

            tbl.set("versions", vt)?;
        }
    }

    Ok(tbl)
}

/// Convert collection-level hooks to a Lua table.
pub(super) fn collection_hooks_to_lua(lua: &Lua, hooks: &Hooks) -> LuaResult<Table> {
    let tbl = lua.create_table()?;
    let pairs: &[(&str, &[String])] = &[
        ("before_validate", &hooks.before_validate),
        ("before_change", &hooks.before_change),
        ("after_change", &hooks.after_change),
        ("before_read", &hooks.before_read),
        ("after_read", &hooks.after_read),
        ("before_delete", &hooks.before_delete),
        ("after_delete", &hooks.after_delete),
        ("before_broadcast", &hooks.before_broadcast),
    ];

    for (key, list) in pairs {
        if !list.is_empty() {
            let arr = lua.create_table()?;
            for (i, s) in list.iter().enumerate() {
                arr.set(i + 1, s.as_str())?;
            }
            tbl.set(*key, arr)?;
        }
    }

    Ok(tbl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        CollectionDefinition,
        collection::{Access, AdminConfig, Hooks, Labels, LiveSetting, McpConfig, VersionsConfig},
        field::{FieldDefinition, FieldType, LocalizedString},
    };
    use mlua::{self, Lua, Value};

    #[test]
    fn test_collection_config_to_lua_basic() {
        let lua = Lua::new();
        let mut def = CollectionDefinition::new("posts");
        def.labels = Labels {
            singular: Some(LocalizedString::Plain("Post".to_string())),
            plural: Some(LocalizedString::Plain("Posts".to_string())),
        };
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .build(),
        ];
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let labels: mlua::Table = tbl.get("labels").unwrap();
        let singular: String = labels.get("singular").unwrap();
        assert_eq!(singular, "Post");
        assert!(tbl.get::<bool>("timestamps").unwrap());

        let fields: mlua::Table = tbl.get("fields").unwrap();
        let f1: mlua::Table = fields.get(1).unwrap();
        let fname: String = f1.get("name").unwrap();
        assert_eq!(fname, "title");
    }

    #[test]
    fn test_collection_config_to_lua_live_settings() {
        let lua = Lua::new();

        // live = None -> true
        let def_none_live = CollectionDefinition::new("t");
        let tbl = collection_config_to_lua(&lua, &def_none_live).unwrap();
        assert!(tbl.get::<bool>("live").unwrap());

        // live = Disabled -> false
        let mut def_disabled = CollectionDefinition::new("t");
        def_disabled.live = Some(LiveSetting::Disabled);
        let tbl = collection_config_to_lua(&lua, &def_disabled).unwrap();
        assert!(!tbl.get::<bool>("live").unwrap());

        // live = Function -> string
        let mut def_func = CollectionDefinition::new("t");
        def_func.live = Some(LiveSetting::Function("hooks.live.filter".to_string()));
        let tbl = collection_config_to_lua(&lua, &def_func).unwrap();
        assert_eq!(tbl.get::<String>("live").unwrap(), "hooks.live.filter");
    }

    #[test]
    fn test_collection_config_to_lua_versions() {
        let lua = Lua::new();

        // versions simple (drafts=true, max=0) -> true
        let mut def = CollectionDefinition::new("t");
        def.versions = Some(VersionsConfig::new(true, 0));
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        assert!(tbl.get::<bool>("versions").unwrap());

        // versions table
        let mut def2 = CollectionDefinition::new("t");
        def2.versions = Some(VersionsConfig::new(false, 100));
        let tbl = collection_config_to_lua(&lua, &def2).unwrap();
        let v: mlua::Table = tbl.get("versions").unwrap();
        assert!(!v.get::<bool>("drafts").unwrap());
        assert_eq!(v.get::<u32>("max_versions").unwrap(), 100);
    }

    #[test]
    fn test_collection_hooks_to_lua() {
        let lua = Lua::new();
        let hooks = Hooks {
            before_validate: vec!["hooks.v".to_string()],
            before_change: vec!["hooks.c1".to_string(), "hooks.c2".to_string()],
            after_change: Vec::new(),
            before_read: Vec::new(),
            after_read: Vec::new(),
            before_delete: Vec::new(),
            after_delete: Vec::new(),
            before_broadcast: vec!["hooks.b".to_string()],
        };
        let tbl = collection_hooks_to_lua(&lua, &hooks).unwrap();
        let bv: mlua::Table = tbl.get("before_validate").unwrap();
        assert_eq!(bv.raw_len(), 1);
        let bc: mlua::Table = tbl.get("before_change").unwrap();
        assert_eq!(bc.raw_len(), 2);
        let bb: mlua::Table = tbl.get("before_broadcast").unwrap();
        assert_eq!(bb.raw_len(), 1);
        // Empty hooks should not have entries
        let ac: Value = tbl.get("after_change").unwrap();
        assert!(matches!(ac, Value::Nil));
    }

    #[test]
    fn test_collection_config_to_lua_with_admin() {
        let lua = Lua::new();
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.admin = AdminConfig {
            use_as_title: Some("title".to_string()),
            default_sort: Some("-created_at".to_string()),
            hidden: true,
            list_searchable_fields: vec!["title".to_string(), "body".to_string()],
        };
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let admin: mlua::Table = tbl.get("admin").unwrap();
        assert_eq!(admin.get::<String>("use_as_title").unwrap(), "title");
        assert_eq!(admin.get::<String>("default_sort").unwrap(), "-created_at");
        assert!(admin.get::<bool>("hidden").unwrap());
        let lsf: mlua::Table = admin.get("list_searchable_fields").unwrap();
        assert_eq!(lsf.raw_len(), 2);
    }

    #[test]
    fn test_collection_config_to_lua_mcp_description() {
        let lua = Lua::new();
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = false;
        def.mcp = McpConfig {
            description: Some("Manages blog posts".to_string()),
        };
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let mcp: mlua::Table = tbl.get("mcp").unwrap();
        assert_eq!(
            mcp.get::<String>("description").unwrap(),
            "Manages blog posts"
        );
    }

    #[test]
    fn test_collection_config_to_lua_soft_delete() {
        let lua = Lua::new();
        let mut def = CollectionDefinition::new("posts");
        def.soft_delete = true;
        def.soft_delete_retention = Some("30d".to_string());
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        assert!(tbl.get::<bool>("soft_delete").unwrap());
        assert_eq!(tbl.get::<String>("soft_delete_retention").unwrap(), "30d");
    }

    #[test]
    fn test_collection_config_to_lua_soft_delete_disabled() {
        let lua = Lua::new();
        let def = CollectionDefinition::new("posts");
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let val: Value = tbl.get("soft_delete").unwrap();
        assert!(
            matches!(val, Value::Nil),
            "soft_delete should not be set when false"
        );
    }

    #[test]
    fn test_collection_config_to_lua_access_trash() {
        let lua = Lua::new();
        let mut def = CollectionDefinition::new("posts");
        def.access = Access {
            delete: Some("access.admin_only".to_string()),
            trash: Some("access.editor".to_string()),
            ..Default::default()
        };
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let access: mlua::Table = tbl.get("access").unwrap();
        assert_eq!(access.get::<String>("delete").unwrap(), "access.admin_only");
        assert_eq!(access.get::<String>("trash").unwrap(), "access.editor");
    }

    #[test]
    fn test_collection_config_to_lua_access_trash_absent() {
        let lua = Lua::new();
        let def = CollectionDefinition::new("posts");
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let access: mlua::Table = tbl.get("access").unwrap();
        let val: Value = access.get("trash").unwrap();
        assert!(
            matches!(val, Value::Nil),
            "trash should not be set when None"
        );
    }
}
