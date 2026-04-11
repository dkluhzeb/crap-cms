//! Parsing functions for global Lua definitions.

use anyhow::Result;
use mlua::{Lua, Table};
use tracing::warn;

use crate::{
    core::{FieldDefinition, collection::GlobalDefinition},
    db::query,
};

use super::shared::*;

/// Parse a Lua table into a `GlobalDefinition`, extracting fields, hooks, and access config.
pub fn parse_global_definition(_lua: &Lua, slug: &str, config: &Table) -> Result<GlobalDefinition> {
    query::validate_slug(slug)?;

    let labels = parse_labels(config);
    let fields = parse_fields_section(config)?;
    let hooks = parse_hooks_section(config)?;
    let access = parse_access_config(config);
    let live = parse_live_setting(config);
    let versions = parse_versions_config(config);
    let mcp = parse_mcp_section(config);

    warn_deep_nesting("Global", slug, &fields);
    warn_global_index_unique(slug, &fields);

    let mut def = GlobalDefinition::new(slug);

    def.labels = labels;
    def.fields = fields;
    def.hooks = hooks;
    def.access = access;
    def.mcp = mcp;
    def.live = live.setting;
    def.live_mode = live.mode;
    def.versions = versions;

    Ok(def)
}

/// Warn about index/unique on global fields (pointless on single-row tables).
fn warn_global_index_unique(slug: &str, fields: &[FieldDefinition]) {
    for field in fields {
        if field.index {
            warn!(
                "Global '{}': field '{}' has index = true, which is ignored for globals (single-row tables)",
                slug, field.name
            );
        }

        if field.unique {
            warn!(
                "Global '{}': field '{}' has unique = true, which is ignored for globals (single-row tables)",
                slug, field.name
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::LocalizedString;
    use mlua::Lua;

    #[test]
    fn test_parse_global_definition_mcp_config() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let mcp_tbl = lua.create_table().unwrap();
        mcp_tbl.set("description", "Site settings").unwrap();
        config.set("mcp", mcp_tbl).unwrap();
        let def = parse_global_definition(&lua, "site_settings", &config).unwrap();
        assert_eq!(def.mcp.description.as_deref(), Some("Site settings"));
    }

    #[test]
    fn test_parse_global_definition_warns_index_unique() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "slug").unwrap();
        field.set("type", "text").unwrap();
        field.set("index", true).unwrap();
        field.set("unique", true).unwrap();
        fields_tbl.set(1, field).unwrap();
        config.set("fields", fields_tbl).unwrap();
        let def = parse_global_definition(&lua, "settings", &config).unwrap();
        assert!(def.fields[0].index);
        assert!(def.fields[0].unique);
    }

    #[test]
    fn test_parse_global_definition_with_labels() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let labels_tbl = lua.create_table().unwrap();
        labels_tbl.set("singular", "Settings").unwrap();
        labels_tbl.set("plural", "Settings").unwrap();
        config.set("labels", labels_tbl).unwrap();
        let def = parse_global_definition(&lua, "site_settings", &config).unwrap();
        match def.labels.singular {
            Some(LocalizedString::Plain(s)) => assert_eq!(s, "Settings"),
            other => panic!("Expected Plain label, got {:?}", other),
        }
    }
}
