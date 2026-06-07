//! Parsing functions for global Lua definitions.

use anyhow::Result;
use mlua::{Lua, Table};
use tracing::warn;

use crate::{
    core::{
        FieldDefinition,
        collection::{GLOBAL_OPERATIONS, GlobalDefinition},
    },
    db::query,
};

use super::helpers::deny_unknown_keys;
use super::shared::{
    parse_access_config, parse_fields_section, parse_hooks_section, parse_labels,
    parse_live_setting, parse_mcp_section, parse_versions_config, validate_shared_nested_keys,
    warn_deep_nesting,
};

/// Every key accepted at the top level of `crap.globals.define(slug, {...})`.
/// Globals are single-row, so they take a subset of the collection keys —
/// no `timestamps`, `admin`, `auth`, `upload`, `indexes`, or soft-delete.
const GLOBAL_CONFIG_KEYS: &[&str] = &[
    "labels", "fields", "hooks", "access", "live", "versions", "mcp",
];

/// Parse a Lua table into a `GlobalDefinition`, extracting fields, hooks, and access config.
///
/// # Errors
///
/// Returns an error if the slug is invalid or any nested
/// fields/hooks/versions spec fails to parse.
pub fn parse_global_definition(lua: &Lua, slug: &str, config: &Table) -> Result<GlobalDefinition> {
    query::validate_slug(slug)?;
    deny_unknown_keys(config, "global", GLOBAL_CONFIG_KEYS)?;
    validate_shared_nested_keys(config)?;

    let labels = parse_labels(config);
    let fields = parse_fields_section(lua, config)?;
    let hooks = parse_hooks_section(config)?;
    let access = parse_access_config(config)?;
    let live = parse_live_setting(config)?;
    let versions = parse_versions_config(config)?;
    let mcp = parse_mcp_section(config, GLOBAL_OPERATIONS)?;

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
    use crate::core::LocalizedString;
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
    fn test_global_unknown_top_level_key_is_rejected() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        // `timestamps` is a collection-only key — invalid on a global.
        config.set("timestamps", true).unwrap();
        let err = parse_global_definition(&lua, "site_settings", &config)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("timestamps"),
            "error should name the offending key: {err}"
        );
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
            other => panic!("Expected Plain label, got {other:?}"),
        }
    }
}
