//! Shared parsing helpers used by both collection and global definition parsers.

use anyhow::Result;
use mlua::{Lua, Result as LuaResult, Table, Value};
use tracing::warn;

use crate::core::{
    FieldDefinition,
    collection::{
        Access, Hooks, IndexDefinition, Labels, LiveMode, LiveSetting, McpConfig, VersionsConfig,
    },
};

use super::{fields::parse_fields, helpers::*};

/// Admin UI max nesting depth for rendering fields (must match `MAX_FIELD_DEPTH` in field_context.rs).
const ADMIN_MAX_FIELD_DEPTH: usize = 5;

/// Compute the maximum nesting depth of a field list.
/// Top-level fields are depth 1, their sub-fields are depth 2, etc.
pub(super) fn max_field_nesting(fields: &[FieldDefinition], current: usize) -> usize {
    let mut max = current;

    for f in fields {
        let sub = current + 1;

        max = max.max(max_field_nesting(&f.fields, sub));

        for block in &f.blocks {
            max = max.max(max_field_nesting(&block.fields, sub));
        }

        for tab in &f.tabs {
            max = max.max(max_field_nesting(&tab.fields, sub));
        }
    }

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

/// Parse the `mcp` subtable from a Lua config table.
pub(super) fn parse_mcp_section(config: &Table) -> McpConfig {
    let Ok(mcp_tbl) = get_table(config, "mcp") else {
        return McpConfig::default();
    };
    McpConfig::new(get_string(&mcp_tbl, "description"))
}

/// Parse result for the `live` config field.
#[derive(Debug)]
pub(super) struct LiveConfig {
    pub setting: Option<LiveSetting>,
    pub mode: LiveMode,
}

pub(super) fn parse_live_setting(config: &Table) -> LiveConfig {
    let val: Value = match config.get("live").ok() {
        Some(v) => v,
        None => {
            return LiveConfig {
                setting: None,
                mode: LiveMode::default(),
            };
        }
    };

    match val {
        Value::Boolean(false) => LiveConfig {
            setting: Some(LiveSetting::Disabled),
            mode: LiveMode::default(),
        },
        Value::Boolean(true) | Value::Nil => LiveConfig {
            setting: None,
            mode: LiveMode::default(),
        },
        Value::String(s) => {
            let func_ref = s.to_str().map(|s| s.to_string()).unwrap_or_default();

            LiveConfig {
                setting: if func_ref.is_empty() {
                    None
                } else {
                    Some(LiveSetting::Function(func_ref))
                },
                mode: LiveMode::default(),
            }
        }
        Value::Table(tbl) => {
            let mode = tbl
                .get::<Value>("mode")
                .ok()
                .and_then(|v| match v {
                    Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
                    _ => None,
                })
                .map(|s| match s.as_str() {
                    "full" => LiveMode::Full,
                    _ => LiveMode::Metadata,
                })
                .unwrap_or_default();

            let filter = tbl
                .get::<Value>("filter")
                .ok()
                .and_then(|v| match v {
                    Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
                    _ => None,
                })
                .filter(|s| !s.is_empty());

            LiveConfig {
                setting: filter.map(LiveSetting::Function),
                mode,
            }
        }
        _ => LiveConfig {
            setting: None,
            mode: LiveMode::default(),
        },
    }
}

/// Parse `versions` from a Lua table.
pub(super) fn parse_versions_config(config: &Table) -> LuaResult<Option<VersionsConfig>> {
    let val: Value = match config.get("versions") {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    match val {
        Value::Boolean(true) => Ok(Some(VersionsConfig::new(true, 0))),
        Value::Boolean(false) | Value::Nil => Ok(None),
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
            .filter_map(|r| r.ok())
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

pub(super) fn parse_access_config(config: &Table) -> Access {
    let Ok(access_tbl) = get_table(config, "access") else {
        return Access::default();
    };
    Access::builder()
        .read(get_string(&access_tbl, "read"))
        .create(get_string(&access_tbl, "create"))
        .update(get_string(&access_tbl, "update"))
        .delete(get_string(&access_tbl, "delete"))
        .trash(get_string(&access_tbl, "trash"))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        collection::LiveSetting,
        field::{BlockDefinition, FieldDefinition, FieldTab, FieldType, LocalizedString},
    };
    use mlua::Lua;

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
        assert!(parse_live_setting(&tbl).setting.is_none());
    }

    #[test]
    fn test_parse_live_setting_true() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", true).unwrap();
        let r = parse_live_setting(&tbl);
        assert!(r.setting.is_none());
        assert_eq!(r.mode, LiveMode::Metadata);
    }

    #[test]
    fn test_parse_live_setting_false() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", false).unwrap();
        assert!(matches!(
            parse_live_setting(&tbl).setting,
            Some(LiveSetting::Disabled)
        ));
    }

    #[test]
    fn test_parse_live_setting_function_ref() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", "hooks.live.filter_published").unwrap();
        match parse_live_setting(&tbl).setting {
            Some(LiveSetting::Function(ref s)) => assert_eq!(s, "hooks.live.filter_published"),
            other => panic!("Expected Function, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_live_setting_empty_string() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", "").unwrap();
        assert!(parse_live_setting(&tbl).setting.is_none());
    }

    #[test]
    fn test_parse_live_setting_table_full_mode() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let live_tbl = lua.create_table().unwrap();
        live_tbl.set("mode", "full").unwrap();
        tbl.set("live", live_tbl).unwrap();
        let r = parse_live_setting(&tbl);
        assert!(r.setting.is_none());
        assert_eq!(r.mode, LiveMode::Full);
    }

    #[test]
    fn test_parse_live_setting_table_with_filter() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let live_tbl = lua.create_table().unwrap();
        live_tbl.set("mode", "full").unwrap();
        live_tbl.set("filter", "hooks.live.check").unwrap();
        tbl.set("live", live_tbl).unwrap();
        let r = parse_live_setting(&tbl);
        assert!(matches!(r.setting, Some(LiveSetting::Function(ref s)) if s == "hooks.live.check"));
        assert_eq!(r.mode, LiveMode::Full);
    }

    #[test]
    fn test_parse_live_setting_other_value() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", 42i64).unwrap();
        assert!(parse_live_setting(&tbl).setting.is_none());
    }

    #[test]
    fn test_parse_access_config_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let access = parse_access_config(&tbl);
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
        let access = parse_access_config(&tbl);
        assert_eq!(access.read.as_deref(), Some("hooks.access.allow_all"));
        assert_eq!(access.create.as_deref(), Some("hooks.access.admin_only"));
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
        let access = parse_access_config(&tbl);
        assert_eq!(access.delete.as_deref(), Some("hooks.access.admin_only"));
        assert_eq!(
            access.trash.as_deref(),
            Some("hooks.access.editor_or_above")
        );
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
                FieldDefinition::builder(format!("level_{}", depth), FieldType::Group)
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
            other => panic!("Expected Plain label, got {:?}", other),
        }
    }
}
