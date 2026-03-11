//! Parsing functions for collection and global Lua definitions.

use anyhow::Result;
use mlua::{Lua, Table, Value};

use crate::core::collection::{
    Access, AdminConfig, CollectionDefinition, GlobalDefinition, IndexDefinition, Labels,
    LiveSetting, VersionsConfig,
};
use crate::core::field::{FieldAdmin, FieldDefinition, FieldType, LocalizedString};

use super::auth::parse_collection_auth;
use super::fields::parse_fields;
use super::helpers::*;
use super::upload::{inject_upload_fields, parse_collection_upload};

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
        tracing::warn!(
            "{} '{}': field nesting depth is {} — the admin UI only renders up to {} levels",
            kind,
            slug,
            depth,
            ADMIN_MAX_FIELD_DEPTH
        );
    }
}

/// Parse a Lua table into a `CollectionDefinition`, extracting fields, hooks, auth, upload, etc.
pub fn parse_collection_definition(
    _lua: &Lua,
    slug: &str,
    config: &Table,
) -> Result<CollectionDefinition> {
    crate::db::query::validate_slug(slug)?;
    let labels = if let Ok(labels_tbl) = get_table(config, "labels") {
        Labels {
            singular: get_localized_string(&labels_tbl, "singular"),
            plural: get_localized_string(&labels_tbl, "plural"),
        }
    } else {
        Labels::default()
    };

    let timestamps = get_bool(config, "timestamps", true);

    let admin = if let Ok(admin_tbl) = get_table(config, "admin") {
        let list_searchable_fields =
            if let Ok(tbl) = get_table(&admin_tbl, "list_searchable_fields") {
                tbl.sequence_values::<String>()
                    .filter_map(|r| r.ok())
                    .collect()
            } else {
                Vec::new()
            };
        AdminConfig {
            use_as_title: get_string(&admin_tbl, "use_as_title"),
            default_sort: get_string(&admin_tbl, "default_sort"),
            hidden: get_bool(&admin_tbl, "hidden", false),
            list_searchable_fields,
        }
    } else {
        AdminConfig::default()
    };

    let mut fields = if let Ok(fields_tbl) = get_table(config, "fields") {
        parse_fields(&fields_tbl)?
    } else {
        Vec::new()
    };

    warn_deep_nesting("Collection", slug, &fields);

    let hooks = if let Ok(hooks_tbl) = get_table(config, "hooks") {
        parse_hooks(&hooks_tbl)?
    } else {
        crate::core::collection::Hooks::default()
    };

    // Parse auth: true | { token_expiry = 3600 }
    let auth = parse_collection_auth(config);

    // Parse upload config
    let upload = parse_collection_upload(config);

    // If upload enabled, auto-inject metadata fields
    if let Some(ref u) = upload {
        if u.enabled {
            inject_upload_fields(&mut fields, u);
        }
    }

    // Parse access control
    let access = parse_access_config(config);

    // Parse live setting: absent=None (enabled), false=Disabled, string=Function
    let live = parse_live_setting(config);

    // Parse versions: true | { drafts = true, max_versions = 100 }
    let versions = parse_versions_config(config);

    // Parse compound indexes: indexes = { { fields = { "a", "b" }, unique = true }, ... }
    let indexes = parse_indexes(config);

    // If auth enabled and no email field defined, inject one at index 0
    if let Some(ref a) = auth {
        if a.enabled && !fields.iter().any(|f| f.name == "email") {
            fields.insert(
                0,
                FieldDefinition::builder("email", FieldType::Email)
                    .required(true)
                    .unique(true)
                    .admin(
                        FieldAdmin::builder()
                            .placeholder(LocalizedString::Plain("user@example.com".to_string()))
                            .build(),
                    )
                    .build(),
            );
        }
    }

    // Parse MCP config
    let mcp = if let Ok(mcp_tbl) = get_table(config, "mcp") {
        crate::core::collection::McpConfig {
            description: get_string(&mcp_tbl, "description"),
        }
    } else {
        Default::default()
    };

    let mut def = CollectionDefinition::new(slug);
    def.labels = labels;
    def.timestamps = timestamps;
    def.fields = fields;
    def.admin = admin;
    def.hooks = hooks;
    def.auth = auth;
    def.upload = upload;
    def.access = access;
    def.mcp = mcp;
    def.live = live;
    def.versions = versions;
    def.indexes = indexes;
    Ok(def)
}

/// Parse a Lua table into a `GlobalDefinition`, extracting fields, hooks, and access config.
pub fn parse_global_definition(_lua: &Lua, slug: &str, config: &Table) -> Result<GlobalDefinition> {
    crate::db::query::validate_slug(slug)?;
    let labels = if let Ok(labels_tbl) = get_table(config, "labels") {
        Labels {
            singular: get_localized_string(&labels_tbl, "singular"),
            plural: get_localized_string(&labels_tbl, "plural"),
        }
    } else {
        Labels::default()
    };

    let fields = if let Ok(fields_tbl) = get_table(config, "fields") {
        parse_fields(&fields_tbl)?
    } else {
        Vec::new()
    };

    warn_deep_nesting("Global", slug, &fields);

    // Warn about index/unique on global fields (pointless on single-row tables)
    for field in &fields {
        if field.index {
            tracing::warn!(
                "Global '{}': field '{}' has index = true, which is ignored for globals (single-row tables)",
                slug,
                field.name
            );
        }
        if field.unique {
            tracing::warn!(
                "Global '{}': field '{}' has unique = true, which is ignored for globals (single-row tables)",
                slug,
                field.name
            );
        }
    }

    let hooks = if let Ok(hooks_tbl) = get_table(config, "hooks") {
        parse_hooks(&hooks_tbl)?
    } else {
        crate::core::collection::Hooks::default()
    };

    let access = parse_access_config(config);
    let live = parse_live_setting(config);
    let versions = parse_versions_config(config);

    // Parse MCP config
    let mcp = if let Ok(mcp_tbl) = get_table(config, "mcp") {
        crate::core::collection::McpConfig {
            description: get_string(&mcp_tbl, "description"),
        }
    } else {
        Default::default()
    };

    let mut def = GlobalDefinition::new(slug);
    def.labels = labels;
    def.fields = fields;
    def.hooks = hooks;
    def.access = access;
    def.mcp = mcp;
    def.live = live;
    def.versions = versions;
    Ok(def)
}

/// Parse the `live` setting from a collection/global Lua config table.
/// - Absent / `true` -> `None` (broadcast all events)
/// - `false` -> `Some(LiveSetting::Disabled)`
/// - String -> `Some(LiveSetting::Function(ref))`
pub(super) fn parse_live_setting(config: &Table) -> Option<LiveSetting> {
    let val: Value = config.get("live").ok()?;
    match val {
        Value::Boolean(false) => Some(LiveSetting::Disabled),
        Value::Boolean(true) | Value::Nil => None,
        Value::String(s) => {
            let func_ref = s.to_str().ok()?.to_string();
            if func_ref.is_empty() {
                None
            } else {
                Some(LiveSetting::Function(func_ref))
            }
        }
        _ => None,
    }
}

/// Parse `versions` from a collection Lua table.
/// - `true` -> VersionsConfig with defaults (drafts=true, no limit)
/// - `false` / absent -> None
/// - `{ drafts = true, max_versions = 100 }` -> VersionsConfig with values
pub(super) fn parse_versions_config(config: &Table) -> Option<VersionsConfig> {
    let val: Value = config.get("versions").ok()?;
    match val {
        Value::Boolean(true) => Some(VersionsConfig::new(true, 0)),
        Value::Boolean(false) | Value::Nil => None,
        Value::Table(tbl) => {
            let drafts = get_bool(&tbl, "drafts", true);
            let max_versions = tbl.get::<u32>("max_versions").unwrap_or(0);
            Some(VersionsConfig::new(drafts, max_versions))
        }
        _ => None,
    }
}

/// Parse `indexes` from a collection Lua table.
/// Each entry is `{ fields = { "col_a", "col_b" }, unique = false }`.
pub(super) fn parse_indexes(config: &Table) -> Vec<IndexDefinition> {
    let tbl = match get_table(config, "indexes") {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut indexes = Vec::new();
    for entry in tbl.sequence_values::<Table>() {
        let entry = match entry {
            Ok(t) => t,
            Err(_) => continue,
        };
        let fields_tbl = match get_table(&entry, "fields") {
            Ok(t) => t,
            Err(_) => continue,
        };
        let fields: Vec<String> = fields_tbl
            .sequence_values::<String>()
            .filter_map(|r| r.ok())
            .collect();
        if fields.is_empty() {
            continue;
        }
        let unique = get_bool(&entry, "unique", false);
        let mut idx = IndexDefinition::new(fields);
        idx.unique = unique;
        indexes.push(idx);
    }
    indexes
}

pub(super) fn parse_access_config(config: &Table) -> Access {
    let access_tbl = match get_table(config, "access") {
        Ok(t) => t,
        Err(_) => return Access::default(),
    };
    Access {
        read: get_string(&access_tbl, "read"),
        create: get_string(&access_tbl, "create"),
        update: get_string(&access_tbl, "update"),
        delete: get_string(&access_tbl, "delete"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{
        BlockDefinition, FieldDefinition, FieldTab, FieldType, LocalizedString,
    };
    use mlua::Lua;

    #[test]
    fn test_parse_versions_config_true() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("versions", true).unwrap();
        let result = parse_versions_config(&tbl);
        assert!(result.is_some());
        let v = result.unwrap();
        assert!(v.drafts);
        assert_eq!(v.max_versions, 0);
    }

    #[test]
    fn test_parse_versions_config_false() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("versions", false).unwrap();
        assert!(parse_versions_config(&tbl).is_none());
    }

    #[test]
    fn test_parse_versions_config_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(parse_versions_config(&tbl).is_none());
    }

    #[test]
    fn test_parse_versions_config_table() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let ver = lua.create_table().unwrap();
        ver.set("drafts", false).unwrap();
        ver.set("max_versions", 50u32).unwrap();
        tbl.set("versions", ver).unwrap();
        let result = parse_versions_config(&tbl);
        assert!(result.is_some());
        let v = result.unwrap();
        assert!(!v.drafts);
        assert_eq!(v.max_versions, 50);
    }

    #[test]
    fn test_parse_versions_config_table_defaults() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let ver = lua.create_table().unwrap();
        tbl.set("versions", ver).unwrap();
        let result = parse_versions_config(&tbl);
        assert!(result.is_some());
        let v = result.unwrap();
        assert!(v.drafts);
        assert_eq!(v.max_versions, 0);
    }

    #[test]
    fn test_parse_live_setting_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(parse_live_setting(&tbl).is_none());
    }

    #[test]
    fn test_parse_live_setting_true() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", true).unwrap();
        assert!(parse_live_setting(&tbl).is_none());
    }

    #[test]
    fn test_parse_live_setting_false() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", false).unwrap();
        let result = parse_live_setting(&tbl);
        assert!(matches!(result, Some(LiveSetting::Disabled)));
    }

    #[test]
    fn test_parse_live_setting_function_ref() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", "hooks.live.filter_published").unwrap();
        let result = parse_live_setting(&tbl);
        match result {
            Some(LiveSetting::Function(ref s)) => {
                assert_eq!(s, "hooks.live.filter_published");
            }
            other => panic!("Expected Function, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_live_setting_empty_string() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", "").unwrap();
        assert!(parse_live_setting(&tbl).is_none());
    }

    #[test]
    fn test_parse_access_config_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let access = parse_access_config(&tbl);
        assert!(access.read.is_none());
        assert!(access.create.is_none());
        assert!(access.update.is_none());
        assert!(access.delete.is_none());
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
        assert!(access.delete.is_none());
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
        let indexes = parse_indexes(&config);
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
        let indexes = parse_indexes(&config);
        assert!(indexes.is_empty(), "Empty fields should be skipped");
    }

    #[test]
    fn test_parse_indexes_absent() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let indexes = parse_indexes(&config);
        assert!(
            indexes.is_empty(),
            "Missing indexes key should return empty"
        );
    }

    #[test]
    fn test_parse_versions_config_other_value_returns_none() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("versions", 42i64).unwrap();
        assert!(parse_versions_config(&tbl).is_none());
    }

    #[test]
    fn test_parse_live_setting_other_value_returns_none() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", 42i64).unwrap();
        assert!(parse_live_setting(&tbl).is_none());
    }

    #[test]
    fn test_parse_collection_definition_admin_list_searchable_fields() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let admin_tbl = lua.create_table().unwrap();
        admin_tbl.set("use_as_title", "title").unwrap();
        let lsf = lua.create_table().unwrap();
        lsf.set(1, "title").unwrap();
        lsf.set(2, "body").unwrap();
        admin_tbl.set("list_searchable_fields", lsf).unwrap();
        config.set("admin", admin_tbl).unwrap();
        let def = parse_collection_definition(&lua, "posts", &config).unwrap();
        assert_eq!(def.admin.use_as_title.as_deref(), Some("title"));
        assert_eq!(def.admin.list_searchable_fields, vec!["title", "body"]);
    }

    #[test]
    fn test_parse_collection_definition_mcp_config() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let mcp_tbl = lua.create_table().unwrap();
        mcp_tbl.set("description", "A collection of posts").unwrap();
        config.set("mcp", mcp_tbl).unwrap();
        let def = parse_collection_definition(&lua, "posts", &config).unwrap();
        assert_eq!(
            def.mcp.description.as_deref(),
            Some("A collection of posts")
        );
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
    fn test_parse_indexes_missing_fields_key_skipped() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let indexes_tbl = lua.create_table().unwrap();
        let idx = lua.create_table().unwrap();
        idx.set("unique", true).unwrap();
        indexes_tbl.set(1, idx).unwrap();
        config.set("indexes", indexes_tbl).unwrap();
        let indexes = parse_indexes(&config);
        assert!(indexes.is_empty());
    }

    #[test]
    fn test_parse_collection_hooks_before_broadcast() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let hooks_tbl = lua.create_table().unwrap();
        let bb = lua.create_table().unwrap();
        bb.set(1, "hooks.filter_broadcast").unwrap();
        hooks_tbl.set("before_broadcast", bb).unwrap();
        config.set("hooks", hooks_tbl).unwrap();
        let def = parse_collection_definition(&lua, "posts", &config).unwrap();
        assert_eq!(def.hooks.before_broadcast, vec!["hooks.filter_broadcast"]);
    }

    #[test]
    fn test_max_field_nesting_via_blocks() {
        let inner = FieldDefinition::builder("text", FieldType::Text).build();
        let block = BlockDefinition::new("para", vec![inner]);
        let outer = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![block])
            .build();
        let depth = max_field_nesting(&[outer], 0);
        assert_eq!(depth, 2);
    }

    #[test]
    fn test_max_field_nesting_via_tabs() {
        let inner = FieldDefinition::builder("bio", FieldType::Textarea).build();
        let tab = FieldTab::new("General", vec![inner]);
        let outer = FieldDefinition::builder("tabs", FieldType::Tabs)
            .tabs(vec![tab])
            .build();
        let depth = max_field_nesting(&[outer], 0);
        assert_eq!(depth, 2);
    }

    #[test]
    fn test_warn_deep_nesting_triggers_for_deep_fields() {
        fn nest(depth: usize) -> FieldDefinition {
            if depth == 0 {
                FieldDefinition::builder("leaf", FieldType::Text).build()
            } else {
                FieldDefinition::builder(format!("level_{}", depth), FieldType::Group)
                    .fields(vec![nest(depth - 1)])
                    .build()
            }
        }
        let deep_field = nest(6);
        warn_deep_nesting("Collection", "test", &[deep_field]);
    }

    #[test]
    fn test_parse_collection_definition_auth_no_email_injection_when_email_exists() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        config.set("auth", true).unwrap();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "email").unwrap();
        field.set("type", "email").unwrap();
        fields_tbl.set(1, field).unwrap();
        config.set("fields", fields_tbl).unwrap();
        let def = parse_collection_definition(&lua, "users", &config).unwrap();
        let email_count = def.fields.iter().filter(|f| f.name == "email").count();
        assert_eq!(email_count, 1);
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
