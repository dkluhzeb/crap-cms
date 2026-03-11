//! Serializers for CollectionDefinition and GlobalDefinition to Lua tables.

use super::auth::collection_auth_to_lua;
use super::fields::field_config_to_lua;
use super::helpers::localized_string_to_lua;
use super::upload::collection_upload_to_lua;
use mlua::{Lua, Table};

/// Convert a CollectionDefinition to a full Lua table compatible with parse_collection_definition().
pub(crate) fn collection_config_to_lua(
    lua: &Lua,
    def: &crate::core::CollectionDefinition,
) -> mlua::Result<Table> {
    let tbl = lua.create_table()?;

    // labels
    let labels = lua.create_table()?;
    if let Some(ref s) = def.labels.singular {
        labels.set("singular", localized_string_to_lua(lua, s)?)?;
    }
    if let Some(ref s) = def.labels.plural {
        labels.set("plural", localized_string_to_lua(lua, s)?)?;
    }
    tbl.set("labels", labels)?;

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

    // fields
    let fields_arr = lua.create_table()?;
    for (i, f) in def.fields.iter().enumerate() {
        fields_arr.set(i + 1, field_config_to_lua(lua, f)?)?;
    }
    tbl.set("fields", fields_arr)?;

    // hooks
    let hooks = collection_hooks_to_lua(lua, &def.hooks)?;
    tbl.set("hooks", hooks)?;

    // access
    let access = lua.create_table()?;
    if let Some(ref s) = def.access.read {
        access.set("read", s.as_str())?;
    }
    if let Some(ref s) = def.access.create {
        access.set("create", s.as_str())?;
    }
    if let Some(ref s) = def.access.update {
        access.set("update", s.as_str())?;
    }
    if let Some(ref s) = def.access.delete {
        access.set("delete", s.as_str())?;
    }
    tbl.set("access", access)?;

    // mcp
    if let Some(ref desc) = def.mcp.description {
        let mcp = lua.create_table()?;
        mcp.set("description", desc.as_str())?;
        tbl.set("mcp", mcp)?;
    }

    // auth
    collection_auth_to_lua(lua, &tbl, def)?;

    // upload
    collection_upload_to_lua(lua, &tbl, def)?;

    // live
    match &def.live {
        None => {
            tbl.set("live", true)?;
        }
        Some(crate::core::collection::LiveSetting::Disabled) => {
            tbl.set("live", false)?;
        }
        Some(crate::core::collection::LiveSetting::Function(s)) => {
            tbl.set("live", s.as_str())?;
        }
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

/// Convert a GlobalDefinition to a full Lua table compatible with parse_global_definition().
pub(crate) fn global_config_to_lua(
    lua: &Lua,
    def: &crate::core::collection::GlobalDefinition,
) -> mlua::Result<Table> {
    let tbl = lua.create_table()?;

    // labels
    let labels = lua.create_table()?;
    if let Some(ref s) = def.labels.singular {
        labels.set("singular", localized_string_to_lua(lua, s)?)?;
    }
    if let Some(ref s) = def.labels.plural {
        labels.set("plural", localized_string_to_lua(lua, s)?)?;
    }
    tbl.set("labels", labels)?;

    // fields
    let fields_arr = lua.create_table()?;
    for (i, f) in def.fields.iter().enumerate() {
        fields_arr.set(i + 1, field_config_to_lua(lua, f)?)?;
    }
    tbl.set("fields", fields_arr)?;

    // hooks
    tbl.set("hooks", collection_hooks_to_lua(lua, &def.hooks)?)?;

    // access
    let access = lua.create_table()?;
    if let Some(ref s) = def.access.read {
        access.set("read", s.as_str())?;
    }
    if let Some(ref s) = def.access.create {
        access.set("create", s.as_str())?;
    }
    if let Some(ref s) = def.access.update {
        access.set("update", s.as_str())?;
    }
    if let Some(ref s) = def.access.delete {
        access.set("delete", s.as_str())?;
    }
    tbl.set("access", access)?;

    // mcp
    if let Some(ref desc) = def.mcp.description {
        let mcp = lua.create_table()?;
        mcp.set("description", desc.as_str())?;
        tbl.set("mcp", mcp)?;
    }

    // live
    match &def.live {
        None => {
            tbl.set("live", true)?;
        }
        Some(crate::core::collection::LiveSetting::Disabled) => {
            tbl.set("live", false)?;
        }
        Some(crate::core::collection::LiveSetting::Function(s)) => {
            tbl.set("live", s.as_str())?;
        }
    }

    Ok(tbl)
}

/// Convert collection-level hooks to a Lua table.
fn collection_hooks_to_lua(
    lua: &Lua,
    hooks: &crate::core::collection::Hooks,
) -> mlua::Result<Table> {
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
    use mlua::{self, Lua, Value};

    #[test]
    fn test_collection_config_to_lua_basic() {
        let lua = Lua::new();
        let mut def = crate::core::CollectionDefinition::new("posts");
        def.labels = crate::core::collection::Labels {
            singular: Some(crate::core::field::LocalizedString::Plain(
                "Post".to_string(),
            )),
            plural: Some(crate::core::field::LocalizedString::Plain(
                "Posts".to_string(),
            )),
        };
        def.timestamps = true;
        def.fields = vec![crate::core::field::FieldDefinition::builder(
            "title",
            crate::core::field::FieldType::Text,
        )
        .required(true)
        .build()];
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let labels: mlua::Table = tbl.get("labels").unwrap();
        let singular: String = labels.get("singular").unwrap();
        assert_eq!(singular, "Post");
        assert_eq!(tbl.get::<bool>("timestamps").unwrap(), true);

        let fields: mlua::Table = tbl.get("fields").unwrap();
        let f1: mlua::Table = fields.get(1).unwrap();
        let fname: String = f1.get("name").unwrap();
        assert_eq!(fname, "title");
    }

    #[test]
    fn test_collection_config_to_lua_live_settings() {
        let lua = Lua::new();

        // live = None -> true
        let def_none_live = crate::core::CollectionDefinition::new("t");
        let tbl = collection_config_to_lua(&lua, &def_none_live).unwrap();
        assert_eq!(tbl.get::<bool>("live").unwrap(), true);

        // live = Disabled -> false
        let mut def_disabled = crate::core::CollectionDefinition::new("t");
        def_disabled.live = Some(crate::core::collection::LiveSetting::Disabled);
        let tbl = collection_config_to_lua(&lua, &def_disabled).unwrap();
        assert_eq!(tbl.get::<bool>("live").unwrap(), false);

        // live = Function -> string
        let mut def_func = crate::core::CollectionDefinition::new("t");
        def_func.live = Some(crate::core::collection::LiveSetting::Function(
            "hooks.live.filter".to_string(),
        ));
        let tbl = collection_config_to_lua(&lua, &def_func).unwrap();
        assert_eq!(tbl.get::<String>("live").unwrap(), "hooks.live.filter");
    }

    #[test]
    fn test_collection_config_to_lua_versions() {
        let lua = Lua::new();

        // versions simple (drafts=true, max=0) -> true
        let mut def = crate::core::CollectionDefinition::new("t");
        def.versions = Some(crate::core::collection::VersionsConfig::new(true, 0));
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        assert_eq!(tbl.get::<bool>("versions").unwrap(), true);

        // versions table
        let mut def2 = crate::core::CollectionDefinition::new("t");
        def2.versions = Some(crate::core::collection::VersionsConfig::new(false, 100));
        let tbl = collection_config_to_lua(&lua, &def2).unwrap();
        let v: mlua::Table = tbl.get("versions").unwrap();
        assert_eq!(v.get::<bool>("drafts").unwrap(), false);
        assert_eq!(v.get::<u32>("max_versions").unwrap(), 100);
    }

    #[test]
    fn test_global_config_to_lua_basic() {
        let lua = Lua::new();
        let mut def = crate::core::collection::GlobalDefinition::new("settings");
        def.labels = crate::core::collection::Labels {
            singular: Some(crate::core::field::LocalizedString::Plain(
                "Settings".to_string(),
            )),
            plural: None,
        };
        def.fields = vec![crate::core::field::FieldDefinition::builder(
            "site_name",
            crate::core::field::FieldType::Text,
        )
        .build()];
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
        let mut def = crate::core::collection::GlobalDefinition::new("settings");
        def.access = crate::core::collection::Access {
            read: Some("hooks.access.allow".to_string()),
            ..Default::default()
        };
        def.live = Some(crate::core::collection::LiveSetting::Disabled);
        let tbl = global_config_to_lua(&lua, &def).unwrap();
        assert_eq!(tbl.get::<bool>("live").unwrap(), false);
        let access: mlua::Table = tbl.get("access").unwrap();
        assert_eq!(access.get::<String>("read").unwrap(), "hooks.access.allow");
    }

    #[test]
    fn test_collection_hooks_to_lua() {
        let lua = Lua::new();
        let hooks = crate::core::collection::Hooks {
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
        let mut def = crate::core::CollectionDefinition::new("posts");
        def.timestamps = true;
        def.admin = crate::core::collection::AdminConfig {
            use_as_title: Some("title".to_string()),
            default_sort: Some("-created_at".to_string()),
            hidden: true,
            list_searchable_fields: vec!["title".to_string(), "body".to_string()],
        };
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let admin: mlua::Table = tbl.get("admin").unwrap();
        assert_eq!(admin.get::<String>("use_as_title").unwrap(), "title");
        assert_eq!(admin.get::<String>("default_sort").unwrap(), "-created_at");
        assert_eq!(admin.get::<bool>("hidden").unwrap(), true);
        let lsf: mlua::Table = admin.get("list_searchable_fields").unwrap();
        assert_eq!(lsf.raw_len(), 2);
    }

    #[test]
    fn test_collection_config_to_lua_mcp_description() {
        let lua = Lua::new();
        let mut def = crate::core::CollectionDefinition::new("posts");
        def.timestamps = false;
        def.mcp = crate::core::collection::McpConfig {
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
    fn test_global_config_to_lua_mcp_description() {
        let lua = Lua::new();
        let mut def = crate::core::collection::GlobalDefinition::new("settings");
        def.mcp = crate::core::collection::McpConfig {
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
    fn test_global_config_to_lua_live_function() {
        let lua = Lua::new();
        let mut def = crate::core::collection::GlobalDefinition::new("settings");
        def.live = Some(crate::core::collection::LiveSetting::Function(
            "hooks.live.settings".to_string(),
        ));
        let tbl = global_config_to_lua(&lua, &def).unwrap();
        assert_eq!(tbl.get::<String>("live").unwrap(), "hooks.live.settings");
    }
}
