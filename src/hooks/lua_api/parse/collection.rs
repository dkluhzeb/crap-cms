//! Parsing functions for collection Lua definitions.

use anyhow::Result;
use mlua::{Lua, Table};

use crate::{
    core::{
        CollectionDefinition, FieldAdmin, FieldDefinition, FieldType, LocalizedString,
        collection::{AdminConfig, Auth, COLLECTION_OPERATIONS},
    },
    db::query,
};

use super::{
    auth::{parse_collection_auth, validate_auth_keys},
    fields::parse_required_locales,
    helpers::{deny_unknown_keys, get_bool, get_string, get_table},
    shared::{
        parse_access_config, parse_fields_section, parse_hooks_section, parse_indexes,
        parse_labels, parse_live_setting, parse_mcp_section, parse_versions_config,
        validate_shared_nested_keys, warn_deep_nesting,
    },
    upload::{inject_upload_fields, parse_collection_upload},
};

/// Keys accepted on a collection's `admin` sub-table (mirrors `AdminConfig`).
const COLLECTION_ADMIN_KEYS: &[&str] = &[
    "use_as_title",
    "default_sort",
    "hidden",
    "list_searchable_fields",
    "list_columns",
];

/// Reject unknown keys in the collection's nested sub-tables: the shared ones
/// plus the collection-only `admin` and `upload` (with its `image_sizes` and
/// `format_options` entries).
fn validate_collection_nested_keys(config: &Table) -> Result<()> {
    validate_shared_nested_keys(config)?;

    if let Ok(admin_tbl) = get_table(config, "admin") {
        deny_unknown_keys(&admin_tbl, "collection admin", COLLECTION_ADMIN_KEYS)?;
    }

    if let Ok(upload_tbl) = get_table(config, "upload") {
        validate_upload_keys(&upload_tbl)?;
    }

    validate_auth_keys(config)?;

    Ok(())
}

/// Reject unknown keys in the `upload` sub-table and its nested `image_sizes`
/// entries and `format_options` (`webp`/`avif` quality tables).
fn validate_upload_keys(upload_tbl: &Table) -> Result<()> {
    deny_unknown_keys(
        upload_tbl,
        "upload",
        &[
            "enabled",
            "mime_types",
            "max_file_size",
            "image_sizes",
            "admin_thumbnail",
            "format_options",
        ],
    )?;

    if let Ok(sizes_tbl) = get_table(upload_tbl, "image_sizes") {
        for entry in sizes_tbl.sequence_values::<Table>().flatten() {
            deny_unknown_keys(&entry, "image_size", &["name", "width", "height", "fit"])?;
        }
    }

    if let Ok(fo_tbl) = get_table(upload_tbl, "format_options") {
        deny_unknown_keys(&fo_tbl, "format_options", &["webp", "avif"])?;

        for fmt in ["webp", "avif"] {
            if let Ok(quality_tbl) = get_table(&fo_tbl, fmt) {
                deny_unknown_keys(&quality_tbl, "format_options entry", &["quality", "queue"])?;
            }
        }
    }

    Ok(())
}

/// Every key accepted at the top level of `crap.collections.define(slug, {...})`.
/// Unknown keys are rejected (parity with the strict jobs config), so typos
/// like `timestamp` or `version` fail loudly instead of being silently ignored.
const COLLECTION_CONFIG_KEYS: &[&str] = &[
    "labels",
    "timestamps",
    "admin",
    "fields",
    "hooks",
    "auth",
    "upload",
    "access",
    "live",
    "versions",
    "indexes",
    "mcp",
    "soft_delete",
    "soft_delete_retention",
    "required_locales",
];

/// Parse the `admin` subtable from a Lua config table.
fn parse_admin_config(config: &Table) -> Result<AdminConfig> {
    let Ok(admin_tbl) = get_table(config, "admin") else {
        return Ok(AdminConfig::default());
    };

    let list_searchable_fields = if let Ok(tbl) = get_table(&admin_tbl, "list_searchable_fields") {
        tbl.sequence_values::<String>()
            .filter_map(std::result::Result::ok)
            .collect()
    } else {
        Vec::new()
    };

    let list_columns = if let Ok(tbl) = get_table(&admin_tbl, "list_columns") {
        tbl.sequence_values::<String>()
            .filter_map(std::result::Result::ok)
            .collect()
    } else {
        Vec::new()
    };

    Ok(AdminConfig::builder()
        .use_as_title(get_string(&admin_tbl, "use_as_title"))
        .default_sort(get_string(&admin_tbl, "default_sort"))
        .hidden(get_bool(&admin_tbl, "hidden", false)?)
        .list_searchable_fields(list_searchable_fields)
        .list_columns(list_columns)
        .build())
}

/// If auth is enabled and no email field exists, inject one at index 0.
fn inject_auth_email_field(auth: Option<&Auth>, fields: &mut Vec<FieldDefinition>) {
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
///
/// # Errors
///
/// Returns an error if the slug is invalid, any required table field is
/// missing, or a nested field/hook/auth/upload spec fails to parse.
pub fn parse_collection_definition(
    lua: &Lua,
    slug: &str,
    config: &Table,
) -> Result<CollectionDefinition> {
    query::validate_slug(slug)?;
    deny_unknown_keys(config, "collection", COLLECTION_CONFIG_KEYS)?;
    validate_collection_nested_keys(config)?;

    let labels = parse_labels(config);
    let timestamps = get_bool(config, "timestamps", true)?;
    let admin = parse_admin_config(config)?;
    let mut fields = parse_fields_section(lua, config)?;
    let hooks = parse_hooks_section(config)?;
    let auth = parse_collection_auth(config);
    let upload = parse_collection_upload(config)?;
    let access = parse_access_config(config)?;
    let live = parse_live_setting(config)?;
    let versions = parse_versions_config(config)?;
    let indexes = parse_indexes(config)?;
    let mcp = parse_mcp_section(config, COLLECTION_OPERATIONS)?;

    warn_deep_nesting("Collection", slug, &fields);

    // If upload enabled, auto-inject metadata fields
    if let Some(ref u) = upload
        && u.enabled
    {
        inject_upload_fields(&mut fields, u);
    }

    // Parse soft delete config
    let soft_delete = get_bool(config, "soft_delete", false)?;
    let soft_delete_retention = if soft_delete {
        get_string(config, "soft_delete_retention")
    } else {
        None
    };

    inject_auth_email_field(auth.as_ref(), &mut fields);

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
    def.required_locales = parse_required_locales(config)?;

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
    fn test_parse_collection_definition_admin_list_columns() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let admin_tbl = lua.create_table().unwrap();
        let lc = lua.create_table().unwrap();
        lc.set(1, "title").unwrap();
        lc.set(2, "_status").unwrap();
        lc.set(3, "created_at").unwrap();
        admin_tbl.set("list_columns", lc).unwrap();
        config.set("admin", admin_tbl).unwrap();
        let def = parse_collection_definition(&lua, "posts", &config).unwrap();
        assert_eq!(
            def.admin.list_columns,
            vec!["title", "_status", "created_at"]
        );
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
    fn test_unknown_top_level_key_is_rejected() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        // `timestamp` (singular) is a typo for `timestamps` — must error, not be ignored.
        config.set("timestamp", false).unwrap();
        let err = parse_collection_definition(&lua, "posts", &config)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("timestamp"),
            "error should name the key: {err}"
        );
        assert!(
            err.contains("timestamps"),
            "error should suggest the closest valid key: {err}"
        );
    }

    #[test]
    fn test_unknown_nested_keys_are_rejected() {
        // access sub-table
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let access = lua.create_table().unwrap();
        access.set("raed", "hooks.access.x").unwrap(); // typo for `read`
        config.set("access", access).unwrap();
        let err = parse_collection_definition(&lua, "posts", &config)
            .unwrap_err()
            .to_string();
        assert!(err.contains("raed"), "{err}");

        // admin sub-table
        let config = lua.create_table().unwrap();
        let admin = lua.create_table().unwrap();
        admin.set("use_title", "name").unwrap(); // not `use_as_title`
        config.set("admin", admin).unwrap();
        assert!(parse_collection_definition(&lua, "posts", &config).is_err());

        // versions sub-table
        let config = lua.create_table().unwrap();
        let versions = lua.create_table().unwrap();
        versions.set("draft", true).unwrap(); // not `drafts`
        config.set("versions", versions).unwrap();
        assert!(parse_collection_definition(&lua, "posts", &config).is_err());

        // index entry
        let config = lua.create_table().unwrap();
        let indexes = lua.create_table().unwrap();
        let entry = lua.create_table().unwrap();
        let cols = lua.create_table().unwrap();
        cols.set(1, "status").unwrap();
        entry.set("fields", cols).unwrap();
        entry.set("uniq", true).unwrap(); // not `unique`
        indexes.set(1, entry).unwrap();
        config.set("indexes", indexes).unwrap();
        assert!(parse_collection_definition(&lua, "posts", &config).is_err());
    }

    #[test]
    fn test_unknown_upload_nested_key_rejected() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let upload = lua.create_table().unwrap();
        let sizes = lua.create_table().unwrap();
        let size = lua.create_table().unwrap();
        size.set("name", "thumb").unwrap();
        size.set("width", 100).unwrap();
        size.set("height", 100).unwrap();
        size.set("crop", "center").unwrap(); // unknown image_size key (it's `fit`)
        sizes.set(1, size).unwrap();
        upload.set("image_sizes", sizes).unwrap();
        config.set("upload", upload).unwrap();
        let err = parse_collection_definition(&lua, "media", &config)
            .unwrap_err()
            .to_string();
        assert!(err.contains("crop"), "{err}");
    }

    #[test]
    fn test_all_documented_keys_are_accepted() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        config.set("timestamps", true).unwrap();
        config.set("soft_delete", true).unwrap();
        config.set("soft_delete_retention", "30d").unwrap();
        // Should parse without an unknown-key error.
        assert!(parse_collection_definition(&lua, "posts", &config).is_ok());
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
