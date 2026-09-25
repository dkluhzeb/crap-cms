//! Parsing functions for global Lua definitions.

use anyhow::{Result, bail};
use mlua::{Lua, Table};

use crate::{
    core::{
        FieldDefinition,
        collection::{Access, GLOBAL_OPERATIONS, GlobalDefinition},
        prefixed_name, walk_leaf_fields,
    },
    db::query,
};

use super::helpers::{deny_unknown_keys, get_table};
use super::shared::{
    COLLECTION_HOOK_KEYS, GLOBAL_HOOK_KEYS, parse_access_config, parse_fields_section,
    parse_hooks_section, parse_labels, parse_live_setting, parse_mcp_section,
    parse_versions_config, validate_shared_nested_keys, warn_access_keys_without_features,
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
    query::reject_reserved_tool_prefix(slug)?;
    deny_unknown_keys(config, "global", GLOBAL_CONFIG_KEYS)?;
    validate_shared_nested_keys(config)?;
    reject_global_only_hook_keys(config, slug)?;

    let labels = parse_labels(config);
    let fields = parse_fields_section(lua, config)?;
    let hooks = parse_hooks_section(config)?;
    let access = parse_access_config(config)?;
    reject_global_only_access_keys(&access, slug)?;
    let live = parse_live_setting(config)?;
    let versions = parse_versions_config(config)?;
    let mcp = parse_mcp_section(config, GLOBAL_OPERATIONS)?;

    warn_deep_nesting("Global", slug, &fields);
    reject_global_index_unique(slug, &fields)?;

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

/// Reject hook events that can never fire on a global. A global is never
/// deleted, so `before_delete` / `after_delete` hooks would silently never
/// run — rejected at load like the delete-side access keys.
fn reject_global_only_hook_keys(config: &Table, slug: &str) -> Result<()> {
    let Ok(hooks) = get_table(config, "hooks") else {
        return Ok(());
    };

    for key in COLLECTION_HOOK_KEYS {
        if GLOBAL_HOOK_KEYS.contains(key) || !hooks.contains_key(*key)? {
            continue;
        }

        bail!(
            "Global '{slug}': hooks.{key} is not supported — a global is never \
             deleted. Supported hooks: {}.",
            GLOBAL_HOOK_KEYS.join(", ")
        );
    }

    Ok(())
}

/// Reject `unique` / `index` on a global's row columns — top-level fields and
/// group sub-fields, through layout wrappers. A global is a single row, so a
/// uniqueness constraint or an index on one of its columns can never do
/// anything; rejected at load like the access keys and hooks that never fire.
/// (An array or blocks field's rows live in their own table and are not
/// covered here.)
fn reject_global_index_unique(slug: &str, fields: &[FieldDefinition]) -> Result<()> {
    walk_leaf_fields(fields, "", false, &mut |field, prefix, _| {
        let Some(key) = [("unique", field.unique), ("index", field.index)]
            .into_iter()
            .find_map(|(key, set)| set.then_some(key))
        else {
            return Ok(());
        };

        bail!(
            "Global '{slug}': field '{}' sets {key} = true, which is not supported — \
             a global is a single row, so the constraint could never apply. Remove it.",
            prefixed_name(prefix, &field.name)
        );
    })
}

#[cfg(test)]
mod tests {
    use mlua::Lua;

    use super::*;
    use crate::{
        core::{FieldType, LocalizedString},
        hooks::lua_api::parse::{ACCESS_KEYS, GLOBAL_ACCESS_KEYS},
    };

    /// Regression: `unique` / `index` on a global's columns only warned while
    /// every other key that can never apply to a single row is rejected. Both
    /// are rejected — on a top-level field and on a group sub-field, through a
    /// layout wrapper — naming the column.
    #[test]
    fn global_rejects_unique_and_index_on_its_row_columns() {
        let unique = vec![
            FieldDefinition::builder("code", FieldType::Text)
                .unique(true)
                .build(),
        ];
        let err = reject_global_index_unique("site", &unique)
            .unwrap_err()
            .to_string();
        assert!(err.contains("'code'") && err.contains("unique"), "{err}");

        let indexed_in_group = vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("seo", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("slug", FieldType::Text)
                                .index(true)
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let err = reject_global_index_unique("site", &indexed_in_group)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("'seo__slug'") && err.contains("index"),
            "{err}"
        );

        let plain = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        assert!(reject_global_index_unique("site", &plain).is_ok());
    }

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
    fn global_access_keys_partition_access_keys() {
        // GLOBAL_ACCESS_KEYS + the four keys `reject_global_only_access_keys`
        // rejects must exactly cover ACCESS_KEYS — a new access key can't be
        // added without deciding whether globals support it.
        let rejected = ["create", "delete", "trash", "unlock"];
        for key in ACCESS_KEYS {
            assert!(
                GLOBAL_ACCESS_KEYS.contains(key) != rejected.contains(key),
                "access key '{key}' must be in exactly one of GLOBAL_ACCESS_KEYS / \
                 the global reject list"
            );
        }
        assert_eq!(ACCESS_KEYS.len(), GLOBAL_ACCESS_KEYS.len() + rejected.len());
    }

    /// Regression: a global accepted `hooks.before_delete` / `after_delete`,
    /// which never fire (a global is never deleted). They are rejected at
    /// load, and the global hook keys are the collection ones minus those two.
    #[test]
    fn global_rejects_delete_hooks() {
        for key in ["before_delete", "after_delete"] {
            let lua = Lua::new();
            let config = lua.create_table().unwrap();
            let hooks = lua.create_table().unwrap();
            hooks.set(key, vec!["hooks.audit"]).unwrap();
            config.set("hooks", hooks).unwrap();

            let err = parse_global_definition(&lua, "site_settings", &config)
                .unwrap_err()
                .to_string();
            assert!(err.contains(&format!("hooks.{key}")), "{err}");
        }

        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let hooks = lua.create_table().unwrap();
        hooks.set("before_change", vec!["hooks.audit"]).unwrap();
        config.set("hooks", hooks).unwrap();
        parse_global_definition(&lua, "site_settings", &config).expect("before_change is valid");

        let rejected = ["before_delete", "after_delete"];
        for key in COLLECTION_HOOK_KEYS {
            assert!(
                GLOBAL_HOOK_KEYS.contains(key) != rejected.contains(key),
                "hook key '{key}' must be in exactly one of GLOBAL_HOOK_KEYS / the reject list"
            );
        }
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
