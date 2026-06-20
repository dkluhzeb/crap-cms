//! Parsing functions for global Lua definitions.

use anyhow::{Result, bail};
use mlua::{Lua, Table};
use tracing::warn;

use crate::{
    core::{
        FieldDefinition,
        collection::{Access, GLOBAL_OPERATIONS, GlobalDefinition},
    },
    db::query,
};

use super::helpers::deny_unknown_keys;
use super::shared::{
    parse_access_config, parse_fields_section, parse_hooks_section, parse_labels,
    parse_live_setting, parse_mcp_section, parse_versions_config, validate_shared_nested_keys,
    warn_access_keys_without_features, warn_deep_nesting,
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
    reject_global_only_access_keys(&access, slug)?;
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

    warn_access_keys_without_features(
        "Global",
        slug,
        &def.access,
        def.has_drafts(),
        false, // globals have no soft_delete / trash view
        def.has_versions(),
    );

    Ok(def)
}

/// Reject access keys that can never fire on a global. A global is a single row
/// with only `get`/`update` operations, so `create`/`delete`/`trash`/`unlock`
/// access functions would silently never run. Rejecting them at load (rather
/// than ignoring) keeps globals consistent with the codebase's strict "no
/// meaningless config" stance and surfaces the mistake to the author.
///
/// `read`, `draft`, `update`, the `versions` toggle, and the `admin`/`mcp`
/// surface gates remain valid — globals support drafts/versions, a
/// published/draft read split, admin-UI pages, and MCP exposure.
fn reject_global_only_access_keys(access: &Access, slug: &str) -> Result<()> {
    for (key, present) in [
        ("create", access.create.is_some()),
        ("delete", access.delete.is_some()),
        ("trash", access.trash.is_some()),
        ("unlock", access.unlock.is_some()),
    ] {
        if present {
            bail!(
                "Global '{slug}': access.{key} is not supported — a global has a \
                 single row with only get/update operations. Use access.read, \
                 access.draft, access.update, or the access.versions toggle."
            );
        }
    }

    Ok(())
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
    fn test_global_rejects_collection_only_access_keys() {
        for key in ["create", "delete", "trash", "unlock"] {
            let lua = Lua::new();
            let config = lua.create_table().unwrap();
            let access = lua.create_table().unwrap();
            access.set(key, "hooks.access.admins").unwrap();
            config.set("access", access).unwrap();

            let err = parse_global_definition(&lua, "site_settings", &config)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(&format!("access.{key}")),
                "global access.{key} must be rejected as unsupported: {err}"
            );
        }
    }

    #[test]
    fn test_global_accepts_read_draft_update_versions_access_keys() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let access = lua.create_table().unwrap();
        access.set("read", "hooks.access.public").unwrap();
        access.set("draft", "hooks.access.editors").unwrap();
        access.set("update", "hooks.access.editors").unwrap();
        access.set("versions", "hooks.access.editors").unwrap();
        access.set("admin", "hooks.access.editors").unwrap();
        access.set("mcp", "hooks.access.editors").unwrap();
        config.set("access", access).unwrap();

        let def = parse_global_definition(&lua, "site_settings", &config).unwrap();
        assert!(def.access.read.is_some());
        assert!(def.access.draft.is_some());
        assert!(def.access.update.is_some());
        assert!(def.access.versions.is_some());
        assert!(def.access.admin.is_some());
        assert!(def.access.mcp.is_some());
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
