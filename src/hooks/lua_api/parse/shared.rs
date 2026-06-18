//! Shared parsing helpers used by both collection and global definition parsers.

use anyhow::{Result, bail};
use mlua::{Lua, Result as LuaResult, Table, Value};
use tracing::warn;

use crate::core::{
    FieldDefinition, HookRef, SchemaStep,
    collection::{
        Access, Hooks, IndexDefinition, Labels, LiveMode, LiveSetting, McpConfig, VersionsConfig,
    },
    walk_all_fields,
};

use super::{
    fields::parse_fields,
    helpers::{
        deny_unknown_keys, get_bool, get_localized_string, get_optional_hook_ref, get_string,
        get_table, parse_hooks,
    },
};

/// Admin UI max nesting depth for rendering fields (must match `MAX_FIELD_DEPTH` in `field_context.rs`).
const ADMIN_MAX_FIELD_DEPTH: usize = 5;

/// Lifecycle hook keys accepted on a collection/global `hooks` sub-table.
pub(super) const COLLECTION_HOOK_KEYS: &[&str] = &[
    "before_validate",
    "before_change",
    "after_change",
    "before_read",
    "after_read",
    "before_delete",
    "after_delete",
    "before_broadcast",
];

/// Access-control operation keys accepted on an `access` sub-table.
pub(super) const ACCESS_KEYS: &[&str] = &[
    "read", "create", "update", "delete", "trash", "draft", "versions",
];

/// Reject unknown keys in the nested sub-tables shared by collections and
/// globals (`labels`, `hooks`, `access`, `mcp`, `versions`, `live`, `indexes`). Each
/// `get_table` quietly skips when the key is absent or not a table (e.g.
/// `versions = true`), so only present sub-tables are validated.
pub(super) fn validate_shared_nested_keys(config: &Table) -> Result<()> {
    if let Ok(t) = get_table(config, "labels") {
        deny_unknown_keys(&t, "labels", &["singular", "plural"])?;
    }
    if let Ok(t) = get_table(config, "hooks") {
        deny_unknown_keys(&t, "hooks", COLLECTION_HOOK_KEYS)?;
    }
    if let Ok(t) = get_table(config, "access") {
        deny_unknown_keys(&t, "access", ACCESS_KEYS)?;
    }
    if let Ok(t) = get_table(config, "mcp") {
        deny_unknown_keys(&t, "mcp", &["description", "operations"])?;
    }
    if let Ok(t) = get_table(config, "versions") {
        deny_unknown_keys(&t, "versions", &["drafts", "max_versions"])?;
    }
    if let Ok(t) = get_table(config, "live") {
        deny_unknown_keys(&t, "live", &["mode", "filter"])?;
    }
    if let Ok(t) = get_table(config, "indexes") {
        for entry in t.sequence_values::<Table>().flatten() {
            deny_unknown_keys(&entry, "index", &["fields", "unique"])?;
        }
    }

    Ok(())
}

/// Compute the maximum nesting depth of a field list.
/// Top-level fields are depth 1, their sub-fields are depth 2, etc. — block
/// and tab steps share their owning field's level, so only container-field
/// ancestors count.
pub(super) fn max_field_nesting(fields: &[FieldDefinition], current: usize) -> usize {
    let mut max = current;

    walk_all_fields(fields, &mut Vec::new(), &mut |_, path| {
        let containers = path
            .iter()
            .filter(|s| matches!(s, SchemaStep::Field(_)))
            .count();
        max = max.max(current + 1 + containers);
    });

    max
}

/// Warn if field nesting exceeds the admin UI rendering limit.
pub(super) fn warn_deep_nesting(kind: &str, slug: &str, fields: &[FieldDefinition]) {
    let depth = max_field_nesting(fields, 0);

    if depth > ADMIN_MAX_FIELD_DEPTH {
        warn!(
            "{} '{}': field nesting depth is {} — the admin UI only renders up to {} levels",
            kind, slug, depth, ADMIN_MAX_FIELD_DEPTH
        );
    }
}

/// Parse the `labels` subtable from a Lua config table.
pub(super) fn parse_labels(config: &Table) -> Labels {
    let Ok(labels_tbl) = get_table(config, "labels") else {
        return Labels::default();
    };
    Labels::new(
        get_localized_string(&labels_tbl, "singular"),
        get_localized_string(&labels_tbl, "plural"),
    )
}

/// Parse the `fields` subtable from a Lua config table.
pub(super) fn parse_fields_section(lua: &Lua, config: &Table) -> Result<Vec<FieldDefinition>> {
    let Ok(fields_tbl) = get_table(config, "fields") else {
        return Ok(Vec::new());
    };
    parse_fields(lua, &fields_tbl)
}

/// Parse the `hooks` subtable from a Lua config table.
pub(super) fn parse_hooks_section(config: &Table) -> Result<Hooks> {
    let Ok(hooks_tbl) = get_table(config, "hooks") else {
        return Ok(Hooks::default());
    };
    parse_hooks(&hooks_tbl)
}

/// Parse the `mcp` subtable from a Lua config table. `valid_operations` is
/// the set of operation keys allowed in `mcp.operations` for this kind of
/// definition (collection vs global).
///
/// # Errors
///
/// Returns an error if `mcp.operations` contains an unknown operation key —
/// matching the strict-schema rejection used everywhere else, so a typo'd
/// override fails at load instead of being silently ignored.
pub(super) fn parse_mcp_section(config: &Table, valid_operations: &[&str]) -> Result<McpConfig> {
    let Ok(mcp_tbl) = get_table(config, "mcp") else {
        return Ok(McpConfig::default());
    };

    let mut mcp = McpConfig::new(get_string(&mcp_tbl, "description"));

    if let Ok(ops_tbl) = get_table(&mcp_tbl, "operations") {
        deny_unknown_keys(&ops_tbl, "mcp.operations", valid_operations)?;

        for (k, v) in ops_tbl.pairs::<String, String>().flatten() {
            mcp.operations.insert(k, v);
        }
    }

    Ok(mcp)
}

/// Parse result for the `live` config field.
#[derive(Debug)]
pub(super) struct LiveConfig {
    pub setting: Option<LiveSetting>,
    pub mode: LiveMode,
}

/// Parse the `live` setting. The table form (`{ mode, filter }`) is strict:
/// an unrecognized `mode` value or an invalid `filter` is a hard error, rather
/// than being silently coerced to the default. `filter` is a hook ref — a bare
/// string or a `{ ref, options }` table (options reach the filter as
/// `ctx.options`). Unknown keys in the table are rejected separately by
/// [`validate_shared_nested_keys`].
///
/// # Errors
///
/// Returns an error if `live.mode` is not `"full"`/`"metadata"`, or if
/// `live.filter` is present but not a valid hook ref.
pub(super) fn parse_live_setting(config: &Table) -> Result<LiveConfig> {
    let val: Value = match config.get("live").ok() {
        Some(v) => v,
        None => {
            return Ok(LiveConfig {
                setting: None,
                mode: LiveMode::default(),
            });
        }
    };

    let cfg = match val {
        Value::Boolean(false) => LiveConfig {
            setting: Some(LiveSetting::Disabled),
            mode: LiveMode::default(),
        },
        Value::String(s) => {
            let func_ref = s.to_str().map(|s| s.to_string()).unwrap_or_default();

            LiveConfig {
                setting: if func_ref.is_empty() {
                    None
                } else {
                    Some(LiveSetting::Function(HookRef::new(func_ref)))
                },
                mode: LiveMode::default(),
            }
        }
        Value::Table(tbl) => {
            let mode = match tbl.get::<Value>("mode")? {
                Value::Nil => LiveMode::default(),
                Value::String(s) => match &*s.to_str()? {
                    "full" => LiveMode::Full,
                    "metadata" => LiveMode::Metadata,
                    other => {
                        bail!("Invalid live.mode value '{other}'. Valid values: full, metadata")
                    }
                },
                other => bail!(
                    "live.mode must be a string (\"full\" or \"metadata\"), got {}",
                    other.type_name()
                ),
            };

            // A bare ref string or a `{ ref, options }` table (options reach
            // the filter as `ctx.options`); an empty ref means "no filter".
            let filter = get_optional_hook_ref(&tbl, "filter", "live.filter")?
                .filter(|h| !h.reference().is_empty());

            LiveConfig {
                setting: filter.map(LiveSetting::Function),
                mode,
            }
        }
        _ => LiveConfig {
            setting: None,
            mode: LiveMode::default(),
        },
    };

    Ok(cfg)
}

/// Parse `versions` from a Lua table.
pub(super) fn parse_versions_config(config: &Table) -> LuaResult<Option<VersionsConfig>> {
    let val: Value = match config.get("versions") {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    match val {
        Value::Boolean(true) => Ok(Some(VersionsConfig::new(true, 0))),
        Value::Table(tbl) => {
            let drafts = get_bool(&tbl, "drafts", true)?;
            let max_versions = tbl.get::<u32>("max_versions").unwrap_or(0);

            Ok(Some(VersionsConfig::new(drafts, max_versions)))
        }
        _ => Ok(None),
    }
}

/// Parse `indexes` from a collection Lua table.
pub(super) fn parse_indexes(config: &Table) -> LuaResult<Vec<IndexDefinition>> {
    let Ok(tbl) = get_table(config, "indexes") else {
        return Ok(Vec::new());
    };
    let mut indexes = Vec::new();

    for entry in tbl.sequence_values::<Table>() {
        let Ok(entry) = entry else { continue };
        let Ok(fields_tbl) = get_table(&entry, "fields") else {
            continue;
        };
        let fields: Vec<String> = fields_tbl
            .sequence_values::<String>()
            .filter_map(std::result::Result::ok)
            .collect();

        if fields.is_empty() {
            continue;
        }

        let unique = get_bool(&entry, "unique", false)?;
        let mut idx = IndexDefinition::new(fields);
        idx.unique = unique;
        indexes.push(idx);
    }

    Ok(indexes)
}

/// Parse the `access` sub-table into [`Access`]. Each rule is a hook reference
/// (a bare string or a `{ ref, options }` table); a present-but-invalid value
/// is a hard error rather than being silently dropped (dropping an access rule
/// the author wrote is a security footgun). Unknown keys are rejected by
/// [`validate_shared_nested_keys`].
///
/// # Errors
///
/// Returns an error if any of `read`/`create`/`update`/`delete`/`trash`/`draft`/
/// `versions` is present but not a valid hook ref.
pub(super) fn parse_access_config(config: &Table) -> Result<Access> {
    let Ok(access_tbl) = get_table(config, "access") else {
        return Ok(Access::default());
    };
    Ok(Access::builder()
        .read(get_optional_hook_ref(&access_tbl, "read", "access")?)
        .create(get_optional_hook_ref(&access_tbl, "create", "access")?)
        .update(get_optional_hook_ref(&access_tbl, "update", "access")?)
        .delete(get_optional_hook_ref(&access_tbl, "delete", "access")?)
        .trash(get_optional_hook_ref(&access_tbl, "trash", "access")?)
        .draft(get_optional_hook_ref(&access_tbl, "draft", "access")?)
        .versions(get_optional_hook_ref(&access_tbl, "versions", "access")?)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::HookRef;
    use crate::core::{
        BlockDefinition, FieldDefinition, FieldTab, FieldType, LocalizedString,
        collection::{COLLECTION_OPERATIONS, LiveSetting},
    };
    use mlua::Lua;

    #[test]
    fn test_parse_mcp_section_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let mcp = parse_mcp_section(&tbl, COLLECTION_OPERATIONS).unwrap();
        assert!(mcp.description.is_none());
        assert!(mcp.operations.is_empty());
    }

    #[test]
    fn test_parse_mcp_section_description_and_operations() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let mcp_tbl = lua.create_table().unwrap();
        mcp_tbl.set("description", "Blog posts").unwrap();

        let ops = lua.create_table().unwrap();
        ops.set("delete", "Purges permanently").unwrap();
        ops.set("create", "Drafts a new post").unwrap();
        mcp_tbl.set("operations", ops).unwrap();
        tbl.set("mcp", mcp_tbl).unwrap();

        let mcp = parse_mcp_section(&tbl, COLLECTION_OPERATIONS).unwrap();
        assert_eq!(mcp.description.as_deref(), Some("Blog posts"));
        assert_eq!(
            mcp.operations.get("delete").map(String::as_str),
            Some("Purges permanently")
        );
        assert_eq!(
            mcp.operations.get("create").map(String::as_str),
            Some("Drafts a new post")
        );
    }

    #[test]
    fn test_parse_mcp_section_rejects_unknown_operation() {
        // A typo'd operation key is a hard error, not a silently-ignored
        // override — parity with the rest of the strict-schema config.
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let mcp_tbl = lua.create_table().unwrap();
        let ops = lua.create_table().unwrap();
        ops.set("delte", "typo").unwrap();
        mcp_tbl.set("operations", ops).unwrap();
        tbl.set("mcp", mcp_tbl).unwrap();

        let err = parse_mcp_section(&tbl, COLLECTION_OPERATIONS)
            .unwrap_err()
            .to_string();
        assert!(err.contains("delte"), "should name the bad key: {err}");
    }

    #[test]
    fn test_parse_versions_config_true() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("versions", true).unwrap();
        let v = parse_versions_config(&tbl).unwrap().unwrap();
        assert!(v.drafts);
        assert_eq!(v.max_versions, 0);
    }

    #[test]
    fn test_parse_versions_config_false() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("versions", false).unwrap();
        assert!(parse_versions_config(&tbl).unwrap().is_none());
    }

    #[test]
    fn test_parse_versions_config_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(parse_versions_config(&tbl).unwrap().is_none());
    }

    #[test]
    fn test_parse_versions_config_table() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let ver = lua.create_table().unwrap();
        ver.set("drafts", false).unwrap();
        ver.set("max_versions", 50u32).unwrap();
        tbl.set("versions", ver).unwrap();
        let v = parse_versions_config(&tbl).unwrap().unwrap();
        assert!(!v.drafts);
        assert_eq!(v.max_versions, 50);
    }

    #[test]
    fn test_parse_versions_config_table_defaults() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let ver = lua.create_table().unwrap();
        tbl.set("versions", ver).unwrap();
        let v = parse_versions_config(&tbl).unwrap().unwrap();
        assert!(v.drafts);
        assert_eq!(v.max_versions, 0);
    }

    #[test]
    fn test_parse_versions_config_other_value() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("versions", 42i64).unwrap();
        assert!(parse_versions_config(&tbl).unwrap().is_none());
    }

    #[test]
    fn test_parse_live_setting_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(parse_live_setting(&tbl).unwrap().setting.is_none());
    }

    #[test]
    fn test_parse_live_setting_true() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", true).unwrap();
        let r = parse_live_setting(&tbl).unwrap();
        assert!(r.setting.is_none());
        assert_eq!(r.mode, LiveMode::Metadata);
    }

    #[test]
    fn test_parse_live_setting_false() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", false).unwrap();
        assert!(matches!(
            parse_live_setting(&tbl).unwrap().setting,
            Some(LiveSetting::Disabled)
        ));
    }

    #[test]
    fn test_parse_live_setting_function_ref() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", "hooks.live.filter_published").unwrap();
        match parse_live_setting(&tbl).unwrap().setting {
            Some(LiveSetting::Function(ref s)) => {
                assert_eq!(s.reference(), "hooks.live.filter_published");
            }
            other => panic!("Expected Function, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_live_setting_empty_string() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", "").unwrap();
        assert!(parse_live_setting(&tbl).unwrap().setting.is_none());
    }

    #[test]
    fn test_parse_live_setting_table_full_mode() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let live_tbl = lua.create_table().unwrap();
        live_tbl.set("mode", "full").unwrap();
        tbl.set("live", live_tbl).unwrap();
        let r = parse_live_setting(&tbl).unwrap();
        assert!(r.setting.is_none());
        assert_eq!(r.mode, LiveMode::Full);
    }

    #[test]
    fn test_parse_live_setting_table_metadata_mode() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let live_tbl = lua.create_table().unwrap();
        live_tbl.set("mode", "metadata").unwrap();
        tbl.set("live", live_tbl).unwrap();
        let r = parse_live_setting(&tbl).unwrap();
        assert_eq!(r.mode, LiveMode::Metadata);
    }

    #[test]
    fn test_parse_live_setting_table_with_filter() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let live_tbl = lua.create_table().unwrap();
        live_tbl.set("mode", "full").unwrap();
        live_tbl.set("filter", "hooks.live.check").unwrap();
        tbl.set("live", live_tbl).unwrap();
        let r = parse_live_setting(&tbl).unwrap();
        assert!(
            matches!(r.setting, Some(LiveSetting::Function(ref s)) if s.reference() == "hooks.live.check")
        );
        assert_eq!(r.mode, LiveMode::Full);
    }

    /// `live.filter` accepts the `{ ref, options }` form — options reach the
    /// filter as `ctx.options`, letting one gate function be reused per
    /// collection with different config.
    #[test]
    fn test_parse_live_setting_filter_with_options() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let live_tbl = lua.create_table().unwrap();
        let filter = lua.create_table().unwrap();
        filter.set("ref", "hooks.live.status_gate").unwrap();
        let opts = lua.create_table().unwrap();
        opts.set("allow", "published").unwrap();
        filter.set("options", opts).unwrap();
        live_tbl.set("filter", filter).unwrap();
        tbl.set("live", live_tbl).unwrap();

        match parse_live_setting(&tbl).unwrap().setting {
            Some(LiveSetting::Function(hook)) => {
                assert_eq!(hook.reference(), "hooks.live.status_gate");
                assert_eq!(
                    hook.options().and_then(|o| o.get("allow")),
                    Some(&serde_json::json!("published"))
                );
            }
            other => panic!("expected Function with options, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_live_setting_other_value() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", 42i64).unwrap();
        assert!(parse_live_setting(&tbl).unwrap().setting.is_none());
    }

    /// Regression: an unrecognized `live.mode` value must be a hard error,
    /// not silently coerced to `metadata` (which would mask a typo and ship
    /// the wrong delivery mode).
    #[test]
    fn test_parse_live_setting_invalid_mode_errors() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let live_tbl = lua.create_table().unwrap();
        live_tbl.set("mode", "fulll").unwrap();
        tbl.set("live", live_tbl).unwrap();
        let err = parse_live_setting(&tbl).unwrap_err().to_string();
        assert!(err.contains("live.mode"), "got: {err}");
        assert!(err.contains("fulll"), "got: {err}");
    }

    /// Regression: a non-string `live.filter` must be a hard error, not
    /// silently dropped (a dropped filter would broadcast events the filter
    /// was meant to suppress).
    #[test]
    fn test_parse_live_setting_non_string_filter_errors() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let live_tbl = lua.create_table().unwrap();
        live_tbl.set("filter", 7i64).unwrap();
        tbl.set("live", live_tbl).unwrap();
        let err = parse_live_setting(&tbl).unwrap_err().to_string();
        assert!(err.contains("live.filter"), "got: {err}");
    }

    /// Regression: an unknown key in the `live` table is rejected by the
    /// shared nested-key validation, matching every other sub-table.
    #[test]
    fn test_validate_live_table_rejects_unknown_key() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let live_tbl = lua.create_table().unwrap();
        live_tbl.set("mode", "full").unwrap();
        live_tbl.set("fitler", "hooks.live.check").unwrap();
        tbl.set("live", live_tbl).unwrap();
        let err = validate_shared_nested_keys(&tbl).unwrap_err().to_string();
        assert!(err.contains("live"), "got: {err}");
        assert!(err.contains("fitler"), "got: {err}");
    }

    #[test]
    fn test_parse_access_config_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let access = parse_access_config(&tbl).unwrap();
        assert!(access.read.is_none());
        assert!(access.create.is_none());
    }

    #[test]
    fn test_parse_access_config_present() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let access_tbl = lua.create_table().unwrap();
        access_tbl.set("read", "hooks.access.allow_all").unwrap();
        access_tbl.set("create", "hooks.access.admin_only").unwrap();
        tbl.set("access", access_tbl).unwrap();
        let access = parse_access_config(&tbl).unwrap();
        assert_eq!(
            access.read.as_ref().map(HookRef::reference),
            Some("hooks.access.allow_all")
        );
        assert_eq!(
            access.create.as_ref().map(HookRef::reference),
            Some("hooks.access.admin_only")
        );
        assert!(access.update.is_none());
    }

    #[test]
    fn test_parse_access_config_trash() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let access_tbl = lua.create_table().unwrap();
        access_tbl.set("delete", "hooks.access.admin_only").unwrap();
        access_tbl
            .set("trash", "hooks.access.editor_or_above")
            .unwrap();
        tbl.set("access", access_tbl).unwrap();
        let access = parse_access_config(&tbl).unwrap();
        assert_eq!(
            access.delete.as_ref().map(HookRef::reference),
            Some("hooks.access.admin_only")
        );
        assert_eq!(
            access.trash.as_ref().map(HookRef::reference),
            Some("hooks.access.editor_or_above")
        );
    }

    /// Regression: `access.draft` and `access.versions` must actually parse.
    /// Both were absent from `parse_access_config` (and `ACCESS_KEYS`), so the
    /// independent-views `draft` gate was unconfigurable from Lua and `draft`/
    /// `versions` keys were rejected as unknown — the whole draft access model
    /// could only be exercised via hand-built Rust structs.
    #[test]
    fn test_parse_access_config_draft_and_versions() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let access_tbl = lua.create_table().unwrap();
        access_tbl.set("draft", "hooks.access.editors").unwrap();
        access_tbl.set("versions", "hooks.access.admins").unwrap();
        tbl.set("access", access_tbl).unwrap();

        let access = parse_access_config(&tbl).unwrap();
        assert_eq!(
            access.draft.as_ref().map(HookRef::reference),
            Some("hooks.access.editors")
        );
        assert_eq!(
            access.versions.as_ref().map(HookRef::reference),
            Some("hooks.access.admins")
        );
    }

    /// `draft` and `versions` are accepted keys on the `access` sub-table — the
    /// strict unknown-key validator must not reject them.
    #[test]
    fn test_access_keys_accept_draft_and_versions() {
        assert!(ACCESS_KEYS.contains(&"draft"));
        assert!(ACCESS_KEYS.contains(&"versions"));
    }

    /// Regression: an access rule that is present but not a string must be a
    /// hard error, not silently dropped — dropping a rule the author wrote is
    /// a security footgun (the field would fall back to the default policy).
    #[test]
    fn test_parse_access_config_non_string_errors() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let access_tbl = lua.create_table().unwrap();
        access_tbl.set("read", true).unwrap();
        tbl.set("access", access_tbl).unwrap();
        let err = parse_access_config(&tbl).unwrap_err().to_string();
        assert!(err.contains("access"), "got: {err}");
        assert!(err.contains("read"), "got: {err}");
    }

    #[test]
    fn test_parse_indexes() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let indexes_tbl = lua.create_table().unwrap();
        let idx1 = lua.create_table().unwrap();
        let fields1 = lua.create_table().unwrap();
        fields1.set(1, "status").unwrap();
        fields1.set(2, "created_at").unwrap();
        idx1.set("fields", fields1).unwrap();
        indexes_tbl.set(1, idx1).unwrap();
        let idx2 = lua.create_table().unwrap();
        let fields2 = lua.create_table().unwrap();
        fields2.set(1, "slug").unwrap();
        idx2.set("fields", fields2).unwrap();
        idx2.set("unique", true).unwrap();
        indexes_tbl.set(2, idx2).unwrap();
        config.set("indexes", indexes_tbl).unwrap();
        let indexes = parse_indexes(&config).unwrap();
        assert_eq!(indexes.len(), 2);
        assert_eq!(indexes[0].fields, vec!["status", "created_at"]);
        assert!(!indexes[0].unique);
        assert_eq!(indexes[1].fields, vec!["slug"]);
        assert!(indexes[1].unique);
    }

    #[test]
    fn test_parse_indexes_empty_fields_skipped() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let indexes_tbl = lua.create_table().unwrap();
        let idx = lua.create_table().unwrap();
        let fields = lua.create_table().unwrap();
        idx.set("fields", fields).unwrap();
        indexes_tbl.set(1, idx).unwrap();
        config.set("indexes", indexes_tbl).unwrap();
        assert!(parse_indexes(&config).unwrap().is_empty());
    }

    #[test]
    fn test_parse_indexes_absent() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        assert!(parse_indexes(&config).unwrap().is_empty());
    }

    #[test]
    fn test_parse_indexes_missing_fields_key_skipped() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let indexes_tbl = lua.create_table().unwrap();
        let idx = lua.create_table().unwrap();
        idx.set("unique", true).unwrap();
        indexes_tbl.set(1, idx).unwrap();
        config.set("indexes", indexes_tbl).unwrap();
        assert!(parse_indexes(&config).unwrap().is_empty());
    }

    #[test]
    fn test_max_field_nesting_via_blocks() {
        let inner = FieldDefinition::builder("text", FieldType::Text).build();
        let block = BlockDefinition::new("para", vec![inner]);
        let outer = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![block])
            .build();
        assert_eq!(max_field_nesting(&[outer], 0), 2);
    }

    #[test]
    fn test_max_field_nesting_via_tabs() {
        let inner = FieldDefinition::builder("bio", FieldType::Textarea).build();
        let tab = FieldTab::new("General", vec![inner]);
        let outer = FieldDefinition::builder("tabs", FieldType::Tabs)
            .tabs(vec![tab])
            .build();
        assert_eq!(max_field_nesting(&[outer], 0), 2);
    }

    #[test]
    fn test_warn_deep_nesting_triggers() {
        fn nest(depth: usize) -> FieldDefinition {
            if depth == 0 {
                FieldDefinition::builder("leaf", FieldType::Text).build()
            } else {
                FieldDefinition::builder(format!("level_{depth}"), FieldType::Group)
                    .fields(vec![nest(depth - 1)])
                    .build()
            }
        }
        warn_deep_nesting("Collection", "test", &[nest(6)]);
    }

    #[test]
    fn test_parse_labels_with_localized() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let labels_tbl = lua.create_table().unwrap();
        labels_tbl.set("singular", "Settings").unwrap();
        labels_tbl.set("plural", "Settings").unwrap();
        config.set("labels", labels_tbl).unwrap();
        let labels = parse_labels(&config);
        match labels.singular {
            Some(LocalizedString::Plain(s)) => assert_eq!(s, "Settings"),
            other => panic!("Expected Plain label, got {other:?}"),
        }
    }
}
