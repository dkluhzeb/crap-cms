//! Parsing functions for collection Lua definitions.

use anyhow::Result;
use mlua::{Lua, Table};

use crate::{
    core::{
        CollectionDefinition, FieldAdmin, FieldDefinition, FieldType, LocalizedString,
        collection::{AdminConfig, Auth},
    },
    db::query,
};

use super::{
    auth::parse_collection_auth,
    helpers::*,
    shared::*,
    upload::{inject_upload_fields, parse_collection_upload},
};

/// Parse the `admin` subtable from a Lua config table.
fn parse_admin_config(config: &Table) -> AdminConfig {
    let Ok(admin_tbl) = get_table(config, "admin") else {
        return AdminConfig::default();
    };

    let list_searchable_fields = if let Ok(tbl) = get_table(&admin_tbl, "list_searchable_fields") {
        tbl.sequence_values::<String>()
            .filter_map(|r| r.ok())
            .collect()
    } else {
        Vec::new()
    };

    AdminConfig::builder()
        .use_as_title(get_string(&admin_tbl, "use_as_title"))
        .default_sort(get_string(&admin_tbl, "default_sort"))
        .hidden(get_bool(&admin_tbl, "hidden", false))
        .list_searchable_fields(list_searchable_fields)
        .build()
}

/// If auth is enabled and no email field exists, inject one at index 0.
fn inject_auth_email_field(auth: &Option<Auth>, fields: &mut Vec<FieldDefinition>) {
    let Some(a) = auth else { return };
    if !a.enabled || fields.iter().any(|f| f.name == "email") {
        return;
    }

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

/// Parse a Lua table into a `CollectionDefinition`, extracting fields, hooks, auth, upload, etc.
pub fn parse_collection_definition(
    _lua: &Lua,
    slug: &str,
    config: &Table,
) -> Result<CollectionDefinition> {
    query::validate_slug(slug)?;

    let labels = parse_labels(config);
    let timestamps = get_bool(config, "timestamps", true);
    let admin = parse_admin_config(config);
    let mut fields = parse_fields_section(config)?;
    let hooks = parse_hooks_section(config)?;
    let auth = parse_collection_auth(config);
    let upload = parse_collection_upload(config);
    let access = parse_access_config(config);
    let live = parse_live_setting(config);
    let versions = parse_versions_config(config);
    let indexes = parse_indexes(config);
    let mcp = parse_mcp_section(config);

    warn_deep_nesting("Collection", slug, &fields);

    // If upload enabled, auto-inject metadata fields
    if let Some(ref u) = upload
        && u.enabled
    {
        inject_upload_fields(&mut fields, u);
    }

    // Parse soft delete config
    let soft_delete = get_bool(config, "soft_delete", false);
    let soft_delete_retention = if soft_delete {
        get_string(config, "soft_delete_retention")
    } else {
        None
    };

    inject_auth_email_field(&auth, &mut fields);

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
    def.live = live.setting;
    def.live_mode = live.mode;
    def.versions = versions;
    def.indexes = indexes;
    def.soft_delete = soft_delete;
    def.soft_delete_retention = soft_delete_retention;

    Ok(def)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

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
    fn test_parse_soft_delete_enabled() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        config.set("soft_delete", true).unwrap();
        config.set("soft_delete_retention", "30d").unwrap();
        let def = parse_collection_definition(&lua, "posts", &config).unwrap();
        assert!(def.soft_delete);
        assert_eq!(def.soft_delete_retention.as_deref(), Some("30d"));
    }

    #[test]
    fn test_parse_soft_delete_disabled_ignores_retention() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        config.set("soft_delete", false).unwrap();
        config.set("soft_delete_retention", "30d").unwrap();
        let def = parse_collection_definition(&lua, "posts", &config).unwrap();
        assert!(!def.soft_delete);
        assert!(def.soft_delete_retention.is_none());
    }

    #[test]
    fn test_parse_soft_delete_absent() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let def = parse_collection_definition(&lua, "posts", &config).unwrap();
        assert!(!def.soft_delete);
        assert!(def.soft_delete_retention.is_none());
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
}
