//! Parsing functions for collection/global Lua definitions into Rust types.

use std::str::FromStr;

use anyhow::Result;
use mlua::{Lua, Table, Value};

use crate::core::{
    field::{FieldType, FieldDefinition, FieldAccess, FieldAdmin, FieldHooks, SelectOption, LocalizedString},
    collection::{AuthStrategy, CollectionAccess, CollectionAuth, CollectionDefinition, GlobalDefinition, CollectionLabels, CollectionAdmin, CollectionHooks, IndexDefinition, LiveSetting, VersionsConfig},
    upload::{CollectionUpload, ImageSize, ImageFit, FormatOptions, FormatQuality},
};

/// Admin UI max nesting depth for rendering fields (must match `MAX_FIELD_DEPTH` in field_context.rs).
const ADMIN_MAX_FIELD_DEPTH: usize = 5;

/// Compute the maximum nesting depth of a field list.
/// Top-level fields are depth 1, their sub-fields are depth 2, etc.
fn max_field_nesting(fields: &[FieldDefinition], current: usize) -> usize {
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
fn warn_deep_nesting(kind: &str, slug: &str, fields: &[FieldDefinition]) {
    let depth = max_field_nesting(fields, 0);
    if depth > ADMIN_MAX_FIELD_DEPTH {
        tracing::warn!(
            "{} '{}': field nesting depth is {} — the admin UI only renders up to {} levels",
            kind, slug, depth, ADMIN_MAX_FIELD_DEPTH
        );
    }
}

/// Parse a Lua table into a `CollectionDefinition`, extracting fields, hooks, auth, upload, etc.
pub fn parse_collection_definition(_lua: &Lua, slug: &str, config: &Table) -> Result<CollectionDefinition> {
    let labels = if let Ok(labels_tbl) = get_table(config, "labels") {
        CollectionLabels {
            singular: get_localized_string(&labels_tbl, "singular"),
            plural: get_localized_string(&labels_tbl, "plural"),
        }
    } else {
        CollectionLabels::default()
    };

    let timestamps = get_bool(config, "timestamps", true);

    let admin = if let Ok(admin_tbl) = get_table(config, "admin") {
        let list_searchable_fields = if let Ok(tbl) = get_table(&admin_tbl, "list_searchable_fields") {
            tbl.sequence_values::<String>().filter_map(|r| r.ok()).collect()
        } else {
            Vec::new()
        };
        CollectionAdmin {
            use_as_title: get_string(&admin_tbl, "use_as_title"),
            default_sort: get_string(&admin_tbl, "default_sort"),
            hidden: get_bool(&admin_tbl, "hidden", false),
            list_searchable_fields,
        }
    } else {
        CollectionAdmin::default()
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
        CollectionHooks::default()
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
            fields.insert(0, FieldDefinition {
                name: "email".to_string(),
                field_type: FieldType::Email,
                required: true,
                unique: true,
                admin: FieldAdmin {
                    placeholder: Some(LocalizedString::Plain("user@example.com".to_string())),
                    ..Default::default()
                },
                ..Default::default()
            });
        }
    }

    // Parse MCP config
    let mcp = if let Ok(mcp_tbl) = get_table(config, "mcp") {
        crate::core::collection::McpCollectionConfig {
            description: get_string(&mcp_tbl, "description"),
        }
    } else {
        Default::default()
    };

    Ok(CollectionDefinition {
        slug: slug.to_string(),
        labels,
        timestamps,
        fields,
        admin,
        hooks,
        auth,
        upload,
        access,
        mcp,
        live,
        versions,
        indexes,
    })
}

/// Parse a Lua table into a `GlobalDefinition`, extracting fields, hooks, and access config.
pub fn parse_global_definition(_lua: &Lua, slug: &str, config: &Table) -> Result<GlobalDefinition> {
    let labels = if let Ok(labels_tbl) = get_table(config, "labels") {
        CollectionLabels {
            singular: get_localized_string(&labels_tbl, "singular"),
            plural: get_localized_string(&labels_tbl, "plural"),
        }
    } else {
        CollectionLabels::default()
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
                slug, field.name
            );
        }
        if field.unique {
            tracing::warn!(
                "Global '{}': field '{}' has unique = true, which is ignored for globals (single-row tables)",
                slug, field.name
            );
        }
    }

    let hooks = if let Ok(hooks_tbl) = get_table(config, "hooks") {
        parse_hooks(&hooks_tbl)?
    } else {
        CollectionHooks::default()
    };

    let access = parse_access_config(config);
    let live = parse_live_setting(config);
    let versions = parse_versions_config(config);

    // Parse MCP config
    let mcp = if let Ok(mcp_tbl) = get_table(config, "mcp") {
        crate::core::collection::McpCollectionConfig {
            description: get_string(&mcp_tbl, "description"),
        }
    } else {
        Default::default()
    };

    Ok(GlobalDefinition {
        slug: slug.to_string(),
        labels,
        fields,
        hooks,
        access,
        mcp,
        live,
        versions,
    })
}

/// Parse the `live` setting from a collection/global Lua config table.
/// - Absent / `true` -> `None` (broadcast all events)
/// - `false` -> `Some(LiveSetting::Disabled)`
/// - String -> `Some(LiveSetting::Function(ref))`
fn parse_live_setting(config: &Table) -> Option<LiveSetting> {
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
/// - `true` → VersionsConfig with defaults (drafts=true, no limit)
/// - `false` / absent → None
/// - `{ drafts = true, max_versions = 100 }` → VersionsConfig with values
fn parse_versions_config(config: &Table) -> Option<VersionsConfig> {
    let val: Value = config.get("versions").ok()?;
    match val {
        Value::Boolean(true) => Some(VersionsConfig {
            drafts: true,
            max_versions: 0,
        }),
        Value::Boolean(false) | Value::Nil => None,
        Value::Table(tbl) => {
            let drafts = get_bool(&tbl, "drafts", true);
            let max_versions = tbl.get::<u32>("max_versions").unwrap_or(0);
            Some(VersionsConfig {
                drafts,
                max_versions,
            })
        }
        _ => None,
    }
}

/// Parse `indexes` from a collection Lua table.
/// Each entry is `{ fields = { "col_a", "col_b" }, unique = false }`.
fn parse_indexes(config: &Table) -> Vec<IndexDefinition> {
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
        indexes.push(IndexDefinition { fields, unique });
    }
    indexes
}

fn parse_access_config(config: &Table) -> CollectionAccess {
    let access_tbl = match get_table(config, "access") {
        Ok(t) => t,
        Err(_) => return CollectionAccess::default(),
    };
    CollectionAccess {
        read: get_string(&access_tbl, "read"),
        create: get_string(&access_tbl, "create"),
        update: get_string(&access_tbl, "update"),
        delete: get_string(&access_tbl, "delete"),
    }
}

fn parse_field_access(access_tbl: &Table) -> FieldAccess {
    FieldAccess {
        read: get_string(access_tbl, "read"),
        create: get_string(access_tbl, "create"),
        update: get_string(access_tbl, "update"),
    }
}

/// Parse the `admin` subtable of a field Lua definition into a `FieldAdmin`.
fn parse_field_admin(admin_tbl: &Table) -> mlua::Result<FieldAdmin> {
    let (labels_singular, labels_plural) = if let Ok(labels_tbl) = get_table(admin_tbl, "labels") {
        (get_localized_string(&labels_tbl, "singular"), get_localized_string(&labels_tbl, "plural"))
    } else {
        (None, None)
    };
    Ok(FieldAdmin {
        label: get_localized_string(admin_tbl, "label"),
        placeholder: get_localized_string(admin_tbl, "placeholder"),
        description: get_localized_string(admin_tbl, "description"),
        hidden: get_bool(admin_tbl, "hidden", false),
        readonly: get_bool(admin_tbl, "readonly", false),
        width: get_string(admin_tbl, "width"),
        collapsed: get_bool(admin_tbl, "collapsed", true),
        label_field: get_string(admin_tbl, "label_field"),
        row_label: get_string(admin_tbl, "row_label"),
        labels_singular,
        labels_plural,
        position: get_string(admin_tbl, "position"),
        condition: get_string(admin_tbl, "condition"),
        step: get_string(admin_tbl, "step"),
        rows: admin_tbl.get::<Option<u32>>("rows").ok().flatten(),
        language: get_string(admin_tbl, "language"),
        features: if let Ok(tbl) = get_table(admin_tbl, "features") {
            tbl.sequence_values::<String>().filter_map(|r| r.ok()).collect()
        } else {
            Vec::new()
        },
        picker: get_string(admin_tbl, "picker"),
        richtext_format: get_string(admin_tbl, "format"),
        nodes: if let Ok(tbl) = get_table(admin_tbl, "nodes") {
            tbl.sequence_values::<String>().filter_map(|r| r.ok()).collect()
        } else {
            Vec::new()
        },
    })
}

/// Parse the relationship config for a field, handling both `Relationship` and `Upload` field types.
/// Returns `None` for field types that don't have a relationship config.
fn parse_field_relationship(field_tbl: &Table, field_type: &FieldType) -> mlua::Result<Option<crate::core::field::RelationshipConfig>> {
    use crate::core::field::RelationshipConfig;
    if *field_type == FieldType::Relationship {
        if let Ok(rel_tbl) = get_table(field_tbl, "relationship") {
            let (collection, polymorphic) = parse_relationship_collection(&rel_tbl);
            let has_many = get_bool(&rel_tbl, "has_many", false);
            let max_depth = rel_tbl.get::<Option<i32>>("max_depth").ok().flatten();
            Ok(Some(RelationshipConfig { collection, has_many, max_depth, polymorphic }))
        } else {
            // Legacy flat syntax: relation_to + has_many on the field itself
            Ok(get_string(field_tbl, "relation_to").map(|collection| {
                let has_many = get_bool(field_tbl, "has_many", false);
                RelationshipConfig { collection, has_many, max_depth: None, polymorphic: vec![] }
            }))
        }
    } else if *field_type == FieldType::Upload {
        // Upload: relationship config from relation_to or relationship table
        if let Ok(rel_tbl) = get_table(field_tbl, "relationship") {
            let collection = get_string(&rel_tbl, "collection").unwrap_or_default();
            let has_many = get_bool(&rel_tbl, "has_many", false);
            let max_depth = rel_tbl.get::<Option<i32>>("max_depth").ok().flatten();
            Ok(Some(RelationshipConfig { collection, has_many, max_depth, polymorphic: vec![] }))
        } else {
            let collection = get_string(field_tbl, "relation_to");
            let has_many = get_bool(field_tbl, "has_many", false);
            Ok(collection.map(|collection| {
                RelationshipConfig { collection, has_many, max_depth: None, polymorphic: vec![] }
            }))
        }
    } else {
        Ok(None)
    }
}

fn parse_collection_auth(config: &Table) -> Option<CollectionAuth> {
    let val: Value = config.get("auth").ok()?;
    match val {
        Value::Boolean(true) => Some(CollectionAuth {
            enabled: true,
            ..Default::default()
        }),
        Value::Boolean(false) | Value::Nil => None,
        Value::Table(tbl) => {
            let token_expiry = tbl.get::<u64>("token_expiry").unwrap_or(7200);
            let disable_local = get_bool(&tbl, "disable_local", false);
            let verify_email = get_bool(&tbl, "verify_email", false);
            let forgot_password = get_bool(&tbl, "forgot_password", true);
            let strategies = parse_auth_strategies(&tbl);
            Some(CollectionAuth {
                enabled: true,
                token_expiry,
                strategies,
                disable_local,
                verify_email,
                forgot_password,
            })
        }
        _ => None,
    }
}

fn parse_auth_strategies(tbl: &Table) -> Vec<AuthStrategy> {
    let strategies_tbl = match get_table(tbl, "strategies") {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut strategies = Vec::new();
    for strat_tbl in strategies_tbl.sequence_values::<Table>().flatten() {
        if let (Some(name), Some(authenticate)) = (
            get_string(&strat_tbl, "name"),
            get_string(&strat_tbl, "authenticate"),
        ) {
            strategies.push(AuthStrategy { name, authenticate });
        }
    }
    strategies
}

fn parse_collection_upload(config: &Table) -> Option<CollectionUpload> {
    let val: Value = config.get("upload").ok()?;
    match val {
        Value::Boolean(true) => Some(CollectionUpload {
            enabled: true,
            ..Default::default()
        }),
        Value::Boolean(false) | Value::Nil => None,
        Value::Table(tbl) => {
            let mime_types = if let Ok(mt_tbl) = get_table(&tbl, "mime_types") {
                mt_tbl.sequence_values::<String>().filter_map(|r| r.ok()).collect()
            } else {
                Vec::new()
            };

            let max_file_size = match tbl.get::<mlua::Value>("max_file_size") {
                Ok(mlua::Value::Integer(n)) => Some(n as u64),
                Ok(mlua::Value::String(s)) => {
                    let text = s.to_str().ok().map(|s| s.to_string());
                    text.and_then(|t| crate::config::parse_filesize_string(&t))
                }
                _ => None,
            };

            let image_sizes = if let Ok(sizes_tbl) = get_table(&tbl, "image_sizes") {
                parse_image_sizes(&sizes_tbl)
            } else {
                Vec::new()
            };

            let admin_thumbnail = get_string(&tbl, "admin_thumbnail");
            let format_options = parse_format_options(&tbl);

            Some(CollectionUpload {
                enabled: true,
                mime_types,
                max_file_size,
                image_sizes,
                admin_thumbnail,
                format_options,
            })
        }
        _ => None,
    }
}

fn parse_image_sizes(tbl: &Table) -> Vec<ImageSize> {
    let mut sizes = Vec::new();
    for size_tbl in tbl.sequence_values::<Table>().flatten() {
        let name = match get_string(&size_tbl, "name") {
            Some(n) => n,
            None => continue,
        };
        let width = size_tbl.get::<u32>("width").unwrap_or(0);
        let height = size_tbl.get::<u32>("height").unwrap_or(0);
        if width == 0 || height == 0 {
            continue;
        }
        let fit = match get_string(&size_tbl, "fit").as_deref() {
            Some("cover") => ImageFit::Cover,
            Some("contain") => ImageFit::Contain,
            Some("inside") => ImageFit::Inside,
            Some("fill") => ImageFit::Fill,
            _ => ImageFit::Cover,
        };
        sizes.push(ImageSize { name, width, height, fit });
    }
    sizes
}

fn parse_format_options(tbl: &Table) -> FormatOptions {
    let fo_tbl = match get_table(tbl, "format_options") {
        Ok(t) => t,
        Err(_) => return FormatOptions::default(),
    };

    let webp = get_table(&fo_tbl, "webp").ok().map(|t| {
        let quality = t.get::<u8>("quality").unwrap_or(80);
        let queue = get_bool(&t, "queue", false);
        FormatQuality { quality, queue }
    });

    let avif = get_table(&fo_tbl, "avif").ok().map(|t| {
        let quality = t.get::<u8>("quality").unwrap_or(60);
        let queue = get_bool(&t, "queue", false);
        FormatQuality { quality, queue }
    });

    FormatOptions { webp, avif }
}

/// Helper to create a hidden text field definition.
fn hidden_text_field(name: &str) -> FieldDefinition {
    FieldDefinition {
        name: name.to_string(),
        admin: FieldAdmin { hidden: true, ..Default::default() },
        ..Default::default()
    }
}

/// Helper to create a hidden number field definition.
fn hidden_number_field(name: &str) -> FieldDefinition {
    FieldDefinition {
        name: name.to_string(),
        field_type: FieldType::Number,
        admin: FieldAdmin { hidden: true, ..Default::default() },
        ..Default::default()
    }
}

/// Auto-inject upload metadata fields at position 0 (before user fields).
/// Generates typed columns for each image size instead of a JSON blob.
fn inject_upload_fields(fields: &mut Vec<FieldDefinition>, upload: &CollectionUpload) {
    let mut upload_fields = vec![
        FieldDefinition {
            name: "filename".to_string(),
            required: true,
            admin: FieldAdmin { readonly: true, ..Default::default() },
            ..Default::default()
        },
        hidden_text_field("mime_type"),
        hidden_number_field("filesize"),
        hidden_number_field("width"),
        hidden_number_field("height"),
        hidden_text_field("url"),
        hidden_number_field("focal_x"),
        hidden_number_field("focal_y"),
    ];

    // Per-size typed fields: {size}_url, {size}_width, {size}_height
    // Plus format variants: {size}_webp_url, {size}_avif_url
    for size in &upload.image_sizes {
        upload_fields.push(hidden_text_field(&format!("{}_url", size.name)));
        upload_fields.push(hidden_number_field(&format!("{}_width", size.name)));
        upload_fields.push(hidden_number_field(&format!("{}_height", size.name)));

        if upload.format_options.webp.is_some() {
            upload_fields.push(hidden_text_field(&format!("{}_webp_url", size.name)));
        }
        if upload.format_options.avif.is_some() {
            upload_fields.push(hidden_text_field(&format!("{}_avif_url", size.name)));
        }
    }

    // Insert at position 0, before user-defined fields
    for (i, field) in upload_fields.into_iter().enumerate() {
        fields.insert(i, field);
    }
}

pub(crate) fn parse_fields(fields_tbl: &Table) -> Result<Vec<FieldDefinition>> {
    let mut fields = Vec::new();

    for pair in fields_tbl.clone().sequence_values::<Table>() {
        let field_tbl = pair?;
        let name: String = get_string_val(&field_tbl, "name")
            .map_err(|_| anyhow::anyhow!("Field missing 'name'"))?;
        let type_str: String = get_string_val(&field_tbl, "type").unwrap_or_else(|_| "text".to_string());
        let field_type = FieldType::from_str(&type_str);

        let required = get_bool(&field_tbl, "required", false);
        let unique = get_bool(&field_tbl, "unique", false);
        let index = get_bool(&field_tbl, "index", false);
        let validate = get_string(&field_tbl, "validate");

        let default_value = {
            let val: Value = field_tbl.get("default_value").unwrap_or(Value::Nil);
            match val {
                Value::Nil => None,
                Value::Boolean(b) => Some(serde_json::Value::Bool(b)),
                Value::Integer(i) => Some(serde_json::Value::Number(serde_json::Number::from(i))),
                Value::Number(n) => serde_json::Number::from_f64(n)
                    .map(serde_json::Value::Number),
                Value::String(s) => Some(serde_json::Value::String(s.to_str()?.to_string())),
                _ => None,
            }
        };

        let options = if let Ok(opts_tbl) = get_table(&field_tbl, "options") {
            parse_select_options(&opts_tbl)?
        } else {
            Vec::new()
        };

        let admin = if let Ok(admin_tbl) = get_table(&field_tbl, "admin") {
            parse_field_admin(&admin_tbl)?
        } else {
            FieldAdmin::default()
        };

        let hooks = if let Ok(hooks_tbl) = get_table(&field_tbl, "hooks") {
            parse_field_hooks(&hooks_tbl)?
        } else {
            FieldHooks::default()
        };

        let access = if let Ok(access_tbl) = get_table(&field_tbl, "access") {
            parse_field_access(&access_tbl)
        } else {
            FieldAccess::default()
        };

        // Parse relationship config
        let relationship = parse_field_relationship(&field_tbl, &field_type)?;

        // Parse sub-fields for Array, Group, Row, and Collapsible types (recursive)
        let sub_fields = if field_type == FieldType::Array || field_type == FieldType::Group || field_type == FieldType::Row || field_type == FieldType::Collapsible {
            if let Ok(sub_fields_tbl) = get_table(&field_tbl, "fields") {
                parse_fields(&sub_fields_tbl)?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let localized = get_bool(&field_tbl, "localized", false);

        // Parse picker_appearance for date fields
        let picker_appearance = if field_type == FieldType::Date {
            get_string(&field_tbl, "picker_appearance")
        } else {
            None
        };

        // Parse block definitions for Blocks type
        let block_defs = if field_type == FieldType::Blocks {
            if let Ok(blocks_tbl) = get_table(&field_tbl, "blocks") {
                parse_block_definitions(&blocks_tbl)?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // Parse tab definitions for Tabs type
        let tab_defs = if field_type == FieldType::Tabs {
            if let Ok(tabs_tbl) = get_table(&field_tbl, "tabs") {
                parse_tab_definitions(&tabs_tbl)?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let min_rows = field_tbl.get::<Option<usize>>("min_rows").ok().flatten();
        let max_rows = field_tbl.get::<Option<usize>>("max_rows").ok().flatten();
        let min_length = field_tbl.get::<Option<usize>>("min_length").ok().flatten();
        let max_length = field_tbl.get::<Option<usize>>("max_length").ok().flatten();
        let min = match field_tbl.get::<mlua::Value>("min") {
            Ok(mlua::Value::Number(n)) => Some(n),
            Ok(mlua::Value::Integer(i)) => Some(i as f64),
            _ => None,
        };
        let max = match field_tbl.get::<mlua::Value>("max") {
            Ok(mlua::Value::Number(n)) => Some(n),
            Ok(mlua::Value::Integer(i)) => Some(i as f64),
            _ => None,
        };

        let has_many = get_bool(&field_tbl, "has_many", false);
        let min_date = get_string(&field_tbl, "min_date");
        let max_date = get_string(&field_tbl, "max_date");

        // Parse join config for Join fields
        let join = if field_type == FieldType::Join {
            let collection = get_string(&field_tbl, "collection").unwrap_or_default();
            let on = get_string(&field_tbl, "on").unwrap_or_default();
            Some(crate::core::field::JoinConfig { collection, on })
        } else {
            None
        };

        // Parse MCP config for field
        let mcp = if let Ok(mcp_tbl) = get_table(&field_tbl, "mcp") {
            crate::core::field::McpFieldConfig {
                description: get_string(&mcp_tbl, "description"),
            }
        } else {
            Default::default()
        };

        fields.push(FieldDefinition {
            name,
            field_type,
            required,
            unique,
            index,
            validate,
            default_value,
            options,
            admin,
            hooks,
            access,
            mcp,
            relationship,
            fields: sub_fields,
            blocks: block_defs,
            tabs: tab_defs,
            localized,
            picker_appearance,
            min_rows,
            max_rows,
            min_length,
            max_length,
            min,
            max,
            has_many,
            min_date,
            max_date,
            join,
        });
    }

    Ok(fields)
}

fn parse_select_options(opts_tbl: &Table) -> Result<Vec<SelectOption>> {
    let mut options = Vec::new();
    for pair in opts_tbl.clone().sequence_values::<Table>() {
        let opt = pair?;
        let label = get_localized_string(&opt, "label")
            .unwrap_or_else(|| LocalizedString::Plain(String::new()));
        let value = get_string_val(&opt, "value").unwrap_or_default();
        options.push(SelectOption { label, value });
    }
    Ok(options)
}

fn parse_field_hooks(hooks_tbl: &Table) -> Result<FieldHooks> {
    Ok(FieldHooks {
        before_validate: parse_string_list(hooks_tbl, "before_validate")?,
        before_change: parse_string_list(hooks_tbl, "before_change")?,
        after_change: parse_string_list(hooks_tbl, "after_change")?,
        after_read: parse_string_list(hooks_tbl, "after_read")?,
    })
}

fn parse_hooks(hooks_tbl: &Table) -> Result<CollectionHooks> {
    Ok(CollectionHooks {
        before_validate: parse_string_list(hooks_tbl, "before_validate")?,
        before_change: parse_string_list(hooks_tbl, "before_change")?,
        after_change: parse_string_list(hooks_tbl, "after_change")?,
        before_read: parse_string_list(hooks_tbl, "before_read")?,
        after_read: parse_string_list(hooks_tbl, "after_read")?,
        before_delete: parse_string_list(hooks_tbl, "before_delete")?,
        after_delete: parse_string_list(hooks_tbl, "after_delete")?,
        before_broadcast: parse_string_list(hooks_tbl, "before_broadcast")?,
    })
}

fn parse_string_list(tbl: &Table, key: &str) -> Result<Vec<String>> {
    if let Ok(list_tbl) = get_table(tbl, key) {
        let mut items = Vec::new();
        for pair in list_tbl.sequence_values::<String>() {
            items.push(pair?);
        }
        Ok(items)
    } else {
        Ok(Vec::new())
    }
}

fn parse_block_definitions(blocks_tbl: &Table) -> Result<Vec<crate::core::field::BlockDefinition>> {
    let mut blocks = Vec::new();
    for entry in blocks_tbl.clone().sequence_values::<Table>() {
        let block_tbl = entry?;
        let block_type: String = get_string_val(&block_tbl, "type")
            .map_err(|_| anyhow::anyhow!("Block definition missing 'type'"))?;
        let label = get_localized_string(&block_tbl, "label");
        let label_field = get_string(&block_tbl, "label_field");
        let group = get_string(&block_tbl, "group");
        let image_url = get_string(&block_tbl, "image_url");
        let fields = if let Ok(fields_tbl) = get_table(&block_tbl, "fields") {
            parse_fields(&fields_tbl)?
        } else {
            Vec::new()
        };
        blocks.push(crate::core::field::BlockDefinition {
            block_type,
            fields,
            label,
            label_field,
            group,
            image_url,
        });
    }
    Ok(blocks)
}

fn parse_tab_definitions(tabs_tbl: &Table) -> Result<Vec<crate::core::field::FieldTab>> {
    let mut tabs = Vec::new();
    for entry in tabs_tbl.clone().sequence_values::<Table>() {
        let tab_tbl = entry?;
        let label = get_string(&tab_tbl, "label").unwrap_or_default();
        let description = get_string(&tab_tbl, "description");
        let fields = if let Ok(fields_tbl) = get_table(&tab_tbl, "fields") {
            parse_fields(&fields_tbl)?
        } else {
            Vec::new()
        };
        tabs.push(crate::core::field::FieldTab { label, description, fields });
    }
    Ok(tabs)
}

// --- Helper functions ---

fn get_table(tbl: &Table, key: &str) -> mlua::Result<Table> {
    tbl.get(key)
}

fn get_string(tbl: &Table, key: &str) -> Option<String> {
    tbl.get::<Option<String>>(key).ok().flatten()
}

/// Parse a Lua value that is either a plain string or a `{locale = string}` table.
fn get_localized_string(tbl: &Table, key: &str) -> Option<LocalizedString> {
    match tbl.get::<Value>(key) {
        Ok(Value::String(s)) => Some(LocalizedString::Plain(s.to_str().ok()?.to_string())),
        Ok(Value::Table(t)) => {
            let mut map = std::collections::HashMap::new();
            for (k, v) in t.pairs::<String, String>().flatten() {
                map.insert(k, v);
            }
            if map.is_empty() { None } else { Some(LocalizedString::Localized(map)) }
        }
        _ => None,
    }
}

fn get_bool(tbl: &Table, key: &str, default: bool) -> bool {
    tbl.get::<Option<bool>>(key).ok().flatten().unwrap_or(default)
}

/// Parse the `collection` field from a relationship Lua table.
///
/// The `collection` key may be:
/// - A plain string → single-collection relationship, returns `(collection, vec![])`.
/// - A Lua array of strings → polymorphic relationship, returns `(first, all_slugs)`.
///   `collection` is set to the first slug; `polymorphic` holds all slugs.
fn parse_relationship_collection(rel_tbl: &Table) -> (String, Vec<String>) {
    match rel_tbl.get::<Value>("collection") {
        Ok(Value::String(s)) => {
            let col = s.to_str().ok().map(|v| v.to_string()).unwrap_or_default();
            (col, vec![])
        }
        Ok(Value::Table(arr)) => {
            let slugs: Vec<String> = arr
                .sequence_values::<String>()
                .filter_map(|r| r.ok())
                .collect();
            let first = slugs.first().cloned().unwrap_or_default();
            (first, slugs)
        }
        _ => (String::new(), vec![]),
    }
}

fn get_string_val(tbl: &Table, key: &str) -> mlua::Result<String> {
    tbl.get(key)
}

/// Parse a Lua table into a `JobDefinition`.
pub fn parse_job_definition(slug: &str, config: &Table) -> Result<crate::core::job::JobDefinition> {
    use crate::core::job::{JobDefinition, JobLabels};

    let handler = get_string(config, "handler")
        .ok_or_else(|| anyhow::anyhow!("Job '{}' missing required 'handler' field", slug))?;

    let schedule = get_string(config, "schedule");

    // Validate cron expression early (the cron crate needs 6-7 fields with seconds;
    // we accept standard 5-field expressions and normalize by prepending "0")
    if let Some(ref expr) = schedule {
        let normalized = {
            let fields: Vec<&str> = expr.split_whitespace().collect();
            if fields.len() == 5 { format!("0 {}", expr) } else { expr.clone() }
        };
        if cron::Schedule::from_str(&normalized).is_err() {
            anyhow::bail!("Job '{}' has invalid cron expression '{}'", slug, expr);
        }
    }

    let queue = get_string(config, "queue").unwrap_or_else(|| "default".to_string());
    let retries = config.get::<Option<u32>>("retries").ok().flatten().unwrap_or(0);
    let timeout = config.get::<Option<u64>>("timeout").ok().flatten().unwrap_or(60);
    let concurrency = config.get::<Option<u32>>("concurrency").ok().flatten().unwrap_or(1);
    let skip_if_running = get_bool(config, "skip_if_running", true);
    let access = get_string(config, "access");

    let labels = if let Ok(labels_tbl) = get_table(config, "labels") {
        JobLabels {
            singular: get_string(&labels_tbl, "singular"),
        }
    } else {
        JobLabels::default()
    };

    Ok(JobDefinition {
        slug: slug.to_string(),
        handler,
        schedule,
        queue,
        retries,
        timeout,
        concurrency,
        skip_if_running,
        labels,
        access,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;
    use crate::core::field::LocalizedString;

    // --- Helper function tests ---

    #[test]
    fn test_get_string_present() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("name", "hello").unwrap();
        assert_eq!(get_string(&tbl, "name"), Some("hello".to_string()));
    }

    #[test]
    fn test_get_string_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert_eq!(get_string(&tbl, "name"), None);
    }

    #[test]
    fn test_get_string_non_string_value() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("num", 42).unwrap();
        // Lua coerces integers to strings, so this returns Some("42")
        assert_eq!(get_string(&tbl, "num"), Some("42".to_string()));

        // Tables and functions cannot be coerced to strings
        let inner = lua.create_table().unwrap();
        tbl.set("tbl", inner).unwrap();
        assert_eq!(get_string(&tbl, "tbl"), None);
    }

    #[test]
    fn test_get_bool_present() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("active", true).unwrap();
        assert!(get_bool(&tbl, "active", false));
    }

    #[test]
    fn test_get_bool_absent_default_true() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(get_bool(&tbl, "active", true));
    }

    #[test]
    fn test_get_bool_absent_default_false() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(!get_bool(&tbl, "active", false));
    }

    #[test]
    fn test_get_string_val_present() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("key", "value").unwrap();
        assert_eq!(get_string_val(&tbl, "key").unwrap(), "value");
    }

    #[test]
    fn test_get_string_val_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(get_string_val(&tbl, "key").is_err());
    }

    #[test]
    fn test_get_table_present() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let inner = lua.create_table().unwrap();
        inner.set("foo", "bar").unwrap();
        tbl.set("inner", inner).unwrap();
        let result = get_table(&tbl, "inner");
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_table_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(get_table(&tbl, "inner").is_err());
    }

    // --- get_localized_string tests ---

    #[test]
    fn test_get_localized_string_plain() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("label", "Hello").unwrap();
        let result = get_localized_string(&tbl, "label");
        match result {
            Some(LocalizedString::Plain(s)) => assert_eq!(s, "Hello"),
            other => panic!("Expected Plain, got {:?}", other),
        }
    }

    #[test]
    fn test_get_localized_string_localized() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let locale_tbl = lua.create_table().unwrap();
        locale_tbl.set("en", "Hello").unwrap();
        locale_tbl.set("de", "Hallo").unwrap();
        tbl.set("label", locale_tbl).unwrap();
        let result = get_localized_string(&tbl, "label");
        match result {
            Some(LocalizedString::Localized(map)) => {
                assert_eq!(map.get("en").unwrap(), "Hello");
                assert_eq!(map.get("de").unwrap(), "Hallo");
            }
            other => panic!("Expected Localized, got {:?}", other),
        }
    }

    #[test]
    fn test_get_localized_string_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        assert!(get_localized_string(&tbl, "label").is_none());
    }

    #[test]
    fn test_get_localized_string_empty_table() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let empty = lua.create_table().unwrap();
        tbl.set("label", empty).unwrap();
        // Empty table returns None
        assert!(get_localized_string(&tbl, "label").is_none());
    }

    // --- parse_job_definition tests ---

    #[test]
    fn test_parse_job_definition_minimal() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("handler", "jobs.my_job.run").unwrap();

        let job = parse_job_definition("my-job", &tbl).unwrap();
        assert_eq!(job.slug, "my-job");
        assert_eq!(job.handler, "jobs.my_job.run");
        assert!(job.schedule.is_none());
        assert_eq!(job.queue, "default");
        assert_eq!(job.retries, 0);
        assert_eq!(job.timeout, 60);
        assert_eq!(job.concurrency, 1);
        assert!(job.skip_if_running);
        assert!(job.access.is_none());
    }

    #[test]
    fn test_parse_job_definition_full() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("handler", "jobs.sync.run").unwrap();
        tbl.set("schedule", "*/5 * * * *").unwrap();
        tbl.set("queue", "sync").unwrap();
        tbl.set("retries", 3u32).unwrap();
        tbl.set("timeout", 300u64).unwrap();
        tbl.set("concurrency", 2u32).unwrap();
        tbl.set("skip_if_running", false).unwrap();
        tbl.set("access", "access.admin_only").unwrap();

        let labels_tbl = lua.create_table().unwrap();
        labels_tbl.set("singular", "Sync Job").unwrap();
        tbl.set("labels", labels_tbl).unwrap();

        let job = parse_job_definition("sync", &tbl).unwrap();
        assert_eq!(job.slug, "sync");
        assert_eq!(job.handler, "jobs.sync.run");
        assert_eq!(job.schedule.as_deref(), Some("*/5 * * * *"));
        assert_eq!(job.queue, "sync");
        assert_eq!(job.retries, 3);
        assert_eq!(job.timeout, 300);
        assert_eq!(job.concurrency, 2);
        assert!(!job.skip_if_running);
        assert_eq!(job.access.as_deref(), Some("access.admin_only"));
        assert_eq!(job.labels.singular.as_deref(), Some("Sync Job"));
    }

    #[test]
    fn test_parse_job_definition_missing_handler() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let result = parse_job_definition("bad-job", &tbl);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing required 'handler'"));
    }

    #[test]
    fn test_parse_job_definition_invalid_cron() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("handler", "jobs.bad.run").unwrap();
        tbl.set("schedule", "not a cron").unwrap();
        let result = parse_job_definition("bad-job", &tbl);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invalid cron expression"));
    }

    // --- parse_versions_config tests ---

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
        // No drafts or max_versions set — should use defaults
        tbl.set("versions", ver).unwrap();
        let result = parse_versions_config(&tbl);
        assert!(result.is_some());
        let v = result.unwrap();
        assert!(v.drafts); // default true
        assert_eq!(v.max_versions, 0); // default 0
    }

    // --- parse_live_setting tests ---

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
        assert!(parse_live_setting(&tbl).is_none()); // true = None = broadcast all
    }

    #[test]
    fn test_parse_live_setting_false() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", false).unwrap();
        let result = parse_live_setting(&tbl);
        assert!(matches!(result, Some(crate::core::collection::LiveSetting::Disabled)));
    }

    #[test]
    fn test_parse_live_setting_function_ref() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("live", "hooks.live.filter_published").unwrap();
        let result = parse_live_setting(&tbl);
        match result {
            Some(crate::core::collection::LiveSetting::Function(ref s)) => {
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
        // Empty string = None (broadcast all)
        assert!(parse_live_setting(&tbl).is_none());
    }

    // --- parse_image_sizes tests ---

    #[test]
    fn test_parse_image_sizes_basic() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("name", "thumbnail").unwrap();
        s1.set("width", 200u32).unwrap();
        s1.set("height", 200u32).unwrap();
        tbl.set(1, s1).unwrap();
        let sizes = parse_image_sizes(&tbl);
        assert_eq!(sizes.len(), 1);
        assert_eq!(sizes[0].name, "thumbnail");
        assert_eq!(sizes[0].width, 200);
        assert_eq!(sizes[0].height, 200);
    }

    #[test]
    fn test_parse_image_sizes_with_fit() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        for (i, (name, fit)) in [("a", "cover"), ("b", "contain"), ("c", "inside"), ("d", "fill")].iter().enumerate() {
            let s = lua.create_table().unwrap();
            s.set("name", *name).unwrap();
            s.set("width", 100u32).unwrap();
            s.set("height", 100u32).unwrap();
            s.set("fit", *fit).unwrap();
            tbl.set(i + 1, s).unwrap();
        }
        let sizes = parse_image_sizes(&tbl);
        assert_eq!(sizes.len(), 4);
        assert!(matches!(sizes[0].fit, crate::core::upload::ImageFit::Cover));
        assert!(matches!(sizes[1].fit, crate::core::upload::ImageFit::Contain));
        assert!(matches!(sizes[2].fit, crate::core::upload::ImageFit::Inside));
        assert!(matches!(sizes[3].fit, crate::core::upload::ImageFit::Fill));
    }

    #[test]
    fn test_parse_image_sizes_skips_missing_name() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        // No name set
        s1.set("width", 200u32).unwrap();
        s1.set("height", 200u32).unwrap();
        tbl.set(1, s1).unwrap();
        let sizes = parse_image_sizes(&tbl);
        assert!(sizes.is_empty());
    }

    #[test]
    fn test_parse_image_sizes_skips_zero_dimensions() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("name", "bad").unwrap();
        s1.set("width", 0u32).unwrap();
        s1.set("height", 200u32).unwrap();
        tbl.set(1, s1).unwrap();
        let sizes = parse_image_sizes(&tbl);
        assert!(sizes.is_empty());
    }

    // --- parse_format_options tests ---

    #[test]
    fn test_parse_format_options_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let fo = parse_format_options(&tbl);
        assert!(fo.webp.is_none());
        assert!(fo.avif.is_none());
    }

    #[test]
    fn test_parse_format_options_webp_only() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let fo_tbl = lua.create_table().unwrap();
        let webp = lua.create_table().unwrap();
        webp.set("quality", 90u8).unwrap();
        fo_tbl.set("webp", webp).unwrap();
        tbl.set("format_options", fo_tbl).unwrap();
        let fo = parse_format_options(&tbl);
        assert!(fo.webp.is_some());
        assert_eq!(fo.webp.unwrap().quality, 90);
        assert!(fo.avif.is_none());
    }

    #[test]
    fn test_parse_format_options_both() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let fo_tbl = lua.create_table().unwrap();
        let webp = lua.create_table().unwrap();
        webp.set("quality", 75u8).unwrap();
        fo_tbl.set("webp", webp).unwrap();
        let avif = lua.create_table().unwrap();
        avif.set("quality", 50u8).unwrap();
        fo_tbl.set("avif", avif).unwrap();
        tbl.set("format_options", fo_tbl).unwrap();
        let fo = parse_format_options(&tbl);
        assert_eq!(fo.webp.unwrap().quality, 75);
        assert_eq!(fo.avif.unwrap().quality, 50);
    }

    // --- inject_upload_fields tests ---

    #[test]
    fn test_inject_upload_fields_basic() {
        let mut fields = vec![FieldDefinition {
            name: "alt_text".to_string(),
            ..Default::default()
        }];
        let upload = crate::core::upload::CollectionUpload {
            enabled: true,
            ..Default::default()
        };
        inject_upload_fields(&mut fields, &upload);
        // Should have base upload fields (filename, mime_type, filesize, width, height, url, focal_x, focal_y) + original alt_text
        assert_eq!(fields.len(), 9); // 8 base + 1 user
        assert_eq!(fields[0].name, "filename");
        assert_eq!(fields[1].name, "mime_type");
        assert_eq!(fields[2].name, "filesize");
        assert_eq!(fields[3].name, "width");
        assert_eq!(fields[4].name, "height");
        assert_eq!(fields[5].name, "url");
        assert_eq!(fields[6].name, "focal_x");
        assert_eq!(fields[7].name, "focal_y");
        assert_eq!(fields[8].name, "alt_text"); // user field pushed to end
    }

    #[test]
    fn test_inject_upload_fields_with_image_sizes() {
        let mut fields = Vec::new();
        let upload = crate::core::upload::CollectionUpload {
            enabled: true,
            image_sizes: vec![
                crate::core::upload::ImageSize {
                    name: "thumb".to_string(),
                    width: 200,
                    height: 200,
                    fit: crate::core::upload::ImageFit::Cover,
                },
            ],
            ..Default::default()
        };
        inject_upload_fields(&mut fields, &upload);
        // 8 base + 3 per-size (thumb_url, thumb_width, thumb_height)
        assert_eq!(fields.len(), 11);
        assert_eq!(fields[8].name, "thumb_url");
        assert_eq!(fields[9].name, "thumb_width");
        assert_eq!(fields[10].name, "thumb_height");
    }

    #[test]
    fn test_inject_upload_fields_with_format_variants() {
        let mut fields = Vec::new();
        let upload = crate::core::upload::CollectionUpload {
            enabled: true,
            image_sizes: vec![
                crate::core::upload::ImageSize {
                    name: "card".to_string(),
                    width: 400,
                    height: 300,
                    fit: crate::core::upload::ImageFit::Cover,
                },
            ],
            format_options: crate::core::upload::FormatOptions {
                webp: Some(crate::core::upload::FormatQuality { quality: 80, queue: false }),
                avif: Some(crate::core::upload::FormatQuality { quality: 60, queue: false }),
            },
            ..Default::default()
        };
        inject_upload_fields(&mut fields, &upload);
        // 8 base + 3 per-size + 2 format variants (card_webp_url, card_avif_url)
        assert_eq!(fields.len(), 13);
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"card_webp_url"));
        assert!(names.contains(&"card_avif_url"));
    }

    // --- parse_collection_auth tests ---

    #[test]
    fn test_parse_collection_auth_true() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("auth", true).unwrap();
        let auth = parse_collection_auth(&tbl);
        assert!(auth.is_some());
        let auth = auth.unwrap();
        assert!(auth.enabled);
        assert_eq!(auth.token_expiry, 7200);
        assert!(!auth.disable_local);
        assert!(!auth.verify_email);
    }

    #[test]
    fn test_parse_collection_auth_false() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("auth", false).unwrap();
        assert!(parse_collection_auth(&tbl).is_none());
    }

    #[test]
    fn test_parse_collection_auth_table() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        auth_tbl.set("token_expiry", 3600u64).unwrap();
        auth_tbl.set("disable_local", true).unwrap();
        auth_tbl.set("verify_email", true).unwrap();
        auth_tbl.set("forgot_password", false).unwrap();
        tbl.set("auth", auth_tbl).unwrap();
        let auth = parse_collection_auth(&tbl);
        assert!(auth.is_some());
        let auth = auth.unwrap();
        assert!(auth.enabled);
        assert_eq!(auth.token_expiry, 3600);
        assert!(auth.disable_local);
        assert!(auth.verify_email);
        assert!(!auth.forgot_password);
    }

    #[test]
    fn test_parse_collection_auth_with_strategies() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        let strats = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("name", "oauth").unwrap();
        s1.set("authenticate", "hooks.auth.oauth_check").unwrap();
        strats.set(1, s1).unwrap();
        auth_tbl.set("strategies", strats).unwrap();
        tbl.set("auth", auth_tbl).unwrap();
        let auth = parse_collection_auth(&tbl).unwrap();
        assert_eq!(auth.strategies.len(), 1);
        assert_eq!(auth.strategies[0].name, "oauth");
        assert_eq!(auth.strategies[0].authenticate, "hooks.auth.oauth_check");
    }

    // --- parse_collection_upload tests ---

    #[test]
    fn test_parse_collection_upload_true() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("upload", true).unwrap();
        let upload = parse_collection_upload(&tbl);
        assert!(upload.is_some());
        assert!(upload.unwrap().enabled);
    }

    #[test]
    fn test_parse_collection_upload_false() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("upload", false).unwrap();
        assert!(parse_collection_upload(&tbl).is_none());
    }

    #[test]
    fn test_parse_collection_upload_table_with_details() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        let mime_types = lua.create_table().unwrap();
        mime_types.set(1, "image/png").unwrap();
        mime_types.set(2, "image/jpeg").unwrap();
        upload_tbl.set("mime_types", mime_types).unwrap();
        upload_tbl.set("max_file_size", 5000000u64).unwrap();
        upload_tbl.set("admin_thumbnail", "thumb").unwrap();

        let sizes = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("name", "thumb").unwrap();
        s1.set("width", 200u32).unwrap();
        s1.set("height", 200u32).unwrap();
        sizes.set(1, s1).unwrap();
        upload_tbl.set("image_sizes", sizes).unwrap();

        tbl.set("upload", upload_tbl).unwrap();
        let upload = parse_collection_upload(&tbl).unwrap();
        assert!(upload.enabled);
        assert_eq!(upload.mime_types, vec!["image/png", "image/jpeg"]);
        assert_eq!(upload.max_file_size, Some(5000000));
        assert_eq!(upload.admin_thumbnail.as_deref(), Some("thumb"));
        assert_eq!(upload.image_sizes.len(), 1);
        assert_eq!(upload.image_sizes[0].name, "thumb");
    }

    // --- parse_access_config tests ---

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

    // --- parse_field_access tests ---

    #[test]
    fn test_parse_field_access() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("read", "hooks.access.check_role").unwrap();
        tbl.set("create", "hooks.access.admin_only").unwrap();
        let access = parse_field_access(&tbl);
        assert_eq!(access.read.as_deref(), Some("hooks.access.check_role"));
        assert_eq!(access.create.as_deref(), Some("hooks.access.admin_only"));
        assert!(access.update.is_none());
    }

    // --- field index + collection indexes parsing tests ---

    #[test]
    fn test_parse_field_index() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "status").unwrap();
        field.set("type", "text").unwrap();
        field.set("index", true).unwrap();
        fields_tbl.set(1, field).unwrap();

        let fields = parse_fields(&fields_tbl).unwrap();
        assert!(fields[0].index, "index should be true");
    }

    #[test]
    fn test_parse_field_index_default_false() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "title").unwrap();
        field.set("type", "text").unwrap();
        fields_tbl.set(1, field).unwrap();

        let fields = parse_fields(&fields_tbl).unwrap();
        assert!(!fields[0].index, "index should default to false");
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
        // Empty fields array
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
        assert!(indexes.is_empty(), "Missing indexes key should return empty");
    }

    // --- parse_collection_upload: max_file_size branches ---

    #[test]
    fn test_parse_collection_upload_max_file_size_integer() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        // Integer value — triggers the Integer branch in max_file_size matching
        upload_tbl.set("max_file_size", 1048576i64).unwrap();
        tbl.set("upload", upload_tbl).unwrap();
        let upload = parse_collection_upload(&tbl).unwrap();
        assert_eq!(upload.max_file_size, Some(1048576));
    }

    #[test]
    fn test_parse_collection_upload_max_file_size_string() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        // String value like "10MB" — triggers the String branch via parse_filesize_string
        upload_tbl.set("max_file_size", "10MB").unwrap();
        tbl.set("upload", upload_tbl).unwrap();
        let upload = parse_collection_upload(&tbl).unwrap();
        assert_eq!(upload.max_file_size, Some(10 * 1024 * 1024));
    }

    #[test]
    fn test_parse_collection_upload_other_value_returns_none() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        // A table value for upload (not bool true/false or Table config) triggers `_ => None`
        let func = lua.create_function(|_, ()| Ok(())).unwrap();
        tbl.set("upload", func).unwrap();
        assert!(parse_collection_upload(&tbl).is_none());
    }

    // --- parse_collection_auth: other Lua type ---

    #[test]
    fn test_parse_collection_auth_other_value_returns_none() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        // Function value triggers `_ => None`
        let func = lua.create_function(|_, ()| Ok(())).unwrap();
        tbl.set("auth", func).unwrap();
        assert!(parse_collection_auth(&tbl).is_none());
    }

    // --- parse_versions_config: other Lua type ---

    #[test]
    fn test_parse_versions_config_other_value_returns_none() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        // Integer is not bool or table — triggers `_ => None`
        tbl.set("versions", 42i64).unwrap();
        assert!(parse_versions_config(&tbl).is_none());
    }

    // --- parse_live_setting: other Lua type ---

    #[test]
    fn test_parse_live_setting_other_value_returns_none() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        // Integer triggers `_ => None`
        tbl.set("live", 42i64).unwrap();
        assert!(parse_live_setting(&tbl).is_none());
    }

    // --- parse_relationship_collection: missing/other type ---

    #[test]
    fn test_parse_relationship_collection_missing() {
        let lua = Lua::new();
        let rel_tbl = lua.create_table().unwrap();
        // No "collection" key — triggers `_ => (String::new(), vec![])`
        let (col, poly) = parse_relationship_collection(&rel_tbl);
        assert_eq!(col, "");
        assert!(poly.is_empty());
    }

    #[test]
    fn test_parse_relationship_collection_array() {
        let lua = Lua::new();
        let rel_tbl = lua.create_table().unwrap();
        let arr = lua.create_table().unwrap();
        arr.set(1, "posts").unwrap();
        arr.set(2, "pages").unwrap();
        rel_tbl.set("collection", arr).unwrap();
        let (col, poly) = parse_relationship_collection(&rel_tbl);
        assert_eq!(col, "posts"); // first slug
        assert_eq!(poly, vec!["posts", "pages"]);
    }

    #[test]
    fn test_parse_relationship_collection_array_empty() {
        let lua = Lua::new();
        let rel_tbl = lua.create_table().unwrap();
        // Empty array — first() returns None, so col is ""
        let arr = lua.create_table().unwrap();
        rel_tbl.set("collection", arr).unwrap();
        let (col, poly) = parse_relationship_collection(&rel_tbl);
        assert_eq!(col, "");
        assert!(poly.is_empty());
    }

    // --- parse_fields: default_value branches ---

    #[test]
    fn test_parse_fields_default_value_boolean() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "active").unwrap();
        field.set("type", "checkbox").unwrap();
        field.set("default_value", true).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].default_value, Some(serde_json::Value::Bool(true)));
    }

    #[test]
    fn test_parse_fields_default_value_integer() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "count").unwrap();
        field.set("type", "number").unwrap();
        field.set("default_value", 42i64).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].default_value, Some(serde_json::Value::Number(42.into())));
    }

    #[test]
    fn test_parse_fields_default_value_float() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "ratio").unwrap();
        field.set("type", "number").unwrap();
        field.set("default_value", 3.14f64).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let dv = fields[0].default_value.as_ref().unwrap();
        assert!(dv.is_number());
    }

    #[test]
    fn test_parse_fields_default_value_table_ignored() {
        // Table-type default_value falls through to `_ => None`
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "data").unwrap();
        field.set("type", "json").unwrap();
        let inner = lua.create_table().unwrap();
        field.set("default_value", inner).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert!(fields[0].default_value.is_none());
    }

    // --- parse_fields: relationship types ---

    #[test]
    fn test_parse_fields_relationship_table_syntax() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "author").unwrap();
        field.set("type", "relationship").unwrap();
        let rel = lua.create_table().unwrap();
        rel.set("collection", "users").unwrap();
        rel.set("has_many", false).unwrap();
        rel.set("max_depth", 2i32).unwrap();
        field.set("relationship", rel).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let rel = fields[0].relationship.as_ref().unwrap();
        assert_eq!(rel.collection, "users");
        assert!(!rel.has_many);
        assert_eq!(rel.max_depth, Some(2));
    }

    #[test]
    fn test_parse_fields_relationship_legacy_flat_syntax() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "author").unwrap();
        field.set("type", "relationship").unwrap();
        field.set("relation_to", "users").unwrap();
        field.set("has_many", true).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let rel = fields[0].relationship.as_ref().unwrap();
        assert_eq!(rel.collection, "users");
        assert!(rel.has_many);
        assert!(rel.max_depth.is_none());
    }

    #[test]
    fn test_parse_fields_relationship_no_relation_to_returns_none() {
        // Legacy flat syntax with no relation_to → relationship is None
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "ref").unwrap();
        field.set("type", "relationship").unwrap();
        // No relation_to, no relationship table
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert!(fields[0].relationship.is_none());
    }

    #[test]
    fn test_parse_fields_upload_relationship_table_syntax() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "avatar").unwrap();
        field.set("type", "upload").unwrap();
        let rel = lua.create_table().unwrap();
        rel.set("collection", "media").unwrap();
        rel.set("has_many", false).unwrap();
        rel.set("max_depth", 1i32).unwrap();
        field.set("relationship", rel).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let r = fields[0].relationship.as_ref().unwrap();
        assert_eq!(r.collection, "media");
        assert_eq!(r.max_depth, Some(1));
    }

    #[test]
    fn test_parse_fields_upload_relationship_flat_syntax() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "avatar").unwrap();
        field.set("type", "upload").unwrap();
        field.set("relation_to", "media").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let r = fields[0].relationship.as_ref().unwrap();
        assert_eq!(r.collection, "media");
    }

    #[test]
    fn test_parse_fields_upload_no_relation_to() {
        // Upload with no relation_to and no relationship table → relationship is None
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "doc").unwrap();
        field.set("type", "upload").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert!(fields[0].relationship.is_none());
    }

    // --- parse_fields: sub-fields for Group, Row, Collapsible ---

    #[test]
    fn test_parse_fields_group_with_subfields() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "meta").unwrap();
        field.set("type", "group").unwrap();
        let sub = lua.create_table().unwrap();
        let sf = lua.create_table().unwrap();
        sf.set("name", "title").unwrap();
        sf.set("type", "text").unwrap();
        sub.set(1, sf).unwrap();
        field.set("fields", sub).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].fields.len(), 1);
        assert_eq!(fields[0].fields[0].name, "title");
    }

    #[test]
    fn test_parse_fields_row_with_subfields() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "layout_row").unwrap();
        field.set("type", "row").unwrap();
        let sub = lua.create_table().unwrap();
        let sf = lua.create_table().unwrap();
        sf.set("name", "first_name").unwrap();
        sf.set("type", "text").unwrap();
        sub.set(1, sf).unwrap();
        field.set("fields", sub).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].fields.len(), 1);
        assert_eq!(fields[0].fields[0].name, "first_name");
    }

    #[test]
    fn test_parse_fields_collapsible_with_subfields() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "advanced").unwrap();
        field.set("type", "collapsible").unwrap();
        let sub = lua.create_table().unwrap();
        let sf = lua.create_table().unwrap();
        sf.set("name", "notes").unwrap();
        sf.set("type", "textarea").unwrap();
        sub.set(1, sf).unwrap();
        field.set("fields", sub).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].fields.len(), 1);
        assert_eq!(fields[0].fields[0].name, "notes");
    }

    // --- parse_fields: picker_appearance for Date ---

    #[test]
    fn test_parse_fields_date_picker_appearance() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "published_at").unwrap();
        field.set("type", "date").unwrap();
        field.set("picker_appearance", "datetime").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].picker_appearance.as_deref(), Some("datetime"));
    }

    #[test]
    fn test_parse_fields_non_date_picker_appearance_ignored() {
        // picker_appearance is only parsed for Date fields; ignored for Text
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "title").unwrap();
        field.set("type", "text").unwrap();
        field.set("picker_appearance", "datetime").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert!(fields[0].picker_appearance.is_none());
    }

    // --- parse_fields: Blocks type ---

    #[test]
    fn test_parse_fields_blocks_type() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "content").unwrap();
        field.set("type", "blocks").unwrap();
        let blocks = lua.create_table().unwrap();
        let block = lua.create_table().unwrap();
        block.set("type", "paragraph").unwrap();
        block.set("label", "Paragraph").unwrap();
        let bfields = lua.create_table().unwrap();
        let bf = lua.create_table().unwrap();
        bf.set("name", "text").unwrap();
        bf.set("type", "textarea").unwrap();
        bfields.set(1, bf).unwrap();
        block.set("fields", bfields).unwrap();
        blocks.set(1, block).unwrap();
        field.set("blocks", blocks).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].blocks.len(), 1);
        assert_eq!(fields[0].blocks[0].block_type, "paragraph");
        assert_eq!(fields[0].blocks[0].fields.len(), 1);
        assert_eq!(fields[0].blocks[0].fields[0].name, "text");
    }

    #[test]
    fn test_parse_block_definitions_optional_fields() {
        // Block with label_field, group, image_url
        let lua = Lua::new();
        let blocks_tbl = lua.create_table().unwrap();
        let block = lua.create_table().unwrap();
        block.set("type", "hero").unwrap();
        block.set("label_field", "headline").unwrap();
        block.set("group", "Layout").unwrap();
        block.set("image_url", "https://example.com/hero.png").unwrap();
        blocks_tbl.set(1, block).unwrap();
        let blocks = parse_block_definitions(&blocks_tbl).unwrap();
        assert_eq!(blocks[0].label_field.as_deref(), Some("headline"));
        assert_eq!(blocks[0].group.as_deref(), Some("Layout"));
        assert_eq!(blocks[0].image_url.as_deref(), Some("https://example.com/hero.png"));
    }

    #[test]
    fn test_parse_block_definitions_missing_type_error() {
        let lua = Lua::new();
        let blocks_tbl = lua.create_table().unwrap();
        let block = lua.create_table().unwrap();
        // No "type" key
        blocks_tbl.set(1, block).unwrap();
        let result = parse_block_definitions(&blocks_tbl);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing 'type'"));
    }

    // --- parse_fields: Tabs type ---

    #[test]
    fn test_parse_fields_tabs_type() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "tabbed_section").unwrap();
        field.set("type", "tabs").unwrap();
        let tabs = lua.create_table().unwrap();
        let tab = lua.create_table().unwrap();
        tab.set("label", "General").unwrap();
        tab.set("description", "General settings").unwrap();
        let tfields = lua.create_table().unwrap();
        let tf = lua.create_table().unwrap();
        tf.set("name", "bio").unwrap();
        tf.set("type", "textarea").unwrap();
        tfields.set(1, tf).unwrap();
        tab.set("fields", tfields).unwrap();
        tabs.set(1, tab).unwrap();
        field.set("tabs", tabs).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].tabs.len(), 1);
        assert_eq!(fields[0].tabs[0].label, "General");
        assert_eq!(fields[0].tabs[0].description.as_deref(), Some("General settings"));
        assert_eq!(fields[0].tabs[0].fields.len(), 1);
    }

    // --- parse_fields: min/max for Number ---

    #[test]
    fn test_parse_fields_min_max_float() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "score").unwrap();
        field.set("type", "number").unwrap();
        field.set("min", 0.0f64).unwrap();
        field.set("max", 100.0f64).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].min, Some(0.0));
        assert_eq!(fields[0].max, Some(100.0));
    }

    #[test]
    fn test_parse_fields_min_max_integer() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "qty").unwrap();
        field.set("type", "number").unwrap();
        // Integer values trigger the Integer branch in min/max matching
        field.set("min", 1i64).unwrap();
        field.set("max", 99i64).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].min, Some(1.0));
        assert_eq!(fields[0].max, Some(99.0));
    }

    // --- parse_fields: Join type ---

    #[test]
    fn test_parse_fields_join_type() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "authored_posts").unwrap();
        field.set("type", "join").unwrap();
        field.set("collection", "posts").unwrap();
        field.set("on", "author").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let join = fields[0].join.as_ref().unwrap();
        assert_eq!(join.collection, "posts");
        assert_eq!(join.on, "author");
    }

    // --- parse_fields: mcp field config ---

    #[test]
    fn test_parse_fields_mcp_config() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "summary").unwrap();
        field.set("type", "text").unwrap();
        let mcp_tbl = lua.create_table().unwrap();
        mcp_tbl.set("description", "A short summary").unwrap();
        field.set("mcp", mcp_tbl).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].mcp.description.as_deref(), Some("A short summary"));
    }

    // --- parse_collection_definition: admin.list_searchable_fields + mcp ---

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
        assert_eq!(def.mcp.description.as_deref(), Some("A collection of posts"));
    }

    // --- parse_global_definition: mcp config + global index/unique warnings ---

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
        // index = true and unique = true on global fields trigger warnings (don't error)
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
        // Should not error — warnings are emitted via tracing
        let def = parse_global_definition(&lua, "settings", &config).unwrap();
        assert!(def.fields[0].index);
        assert!(def.fields[0].unique);
    }

    // --- parse_format_options: queue field + avif-only ---

    #[test]
    fn test_parse_format_options_queue_flag() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let fo_tbl = lua.create_table().unwrap();
        let webp = lua.create_table().unwrap();
        webp.set("quality", 85u8).unwrap();
        webp.set("queue", true).unwrap();
        fo_tbl.set("webp", webp).unwrap();
        let avif = lua.create_table().unwrap();
        avif.set("quality", 65u8).unwrap();
        avif.set("queue", true).unwrap();
        fo_tbl.set("avif", avif).unwrap();
        tbl.set("format_options", fo_tbl).unwrap();
        let fo = parse_format_options(&tbl);
        assert!(fo.webp.as_ref().unwrap().queue);
        assert_eq!(fo.webp.as_ref().unwrap().quality, 85);
        assert!(fo.avif.as_ref().unwrap().queue);
        assert_eq!(fo.avif.as_ref().unwrap().quality, 65);
    }

    #[test]
    fn test_parse_format_options_avif_only() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let fo_tbl = lua.create_table().unwrap();
        let avif = lua.create_table().unwrap();
        avif.set("quality", 55u8).unwrap();
        fo_tbl.set("avif", avif).unwrap();
        tbl.set("format_options", fo_tbl).unwrap();
        let fo = parse_format_options(&tbl);
        assert!(fo.webp.is_none());
        assert_eq!(fo.avif.as_ref().unwrap().quality, 55);
    }

    // --- parse_image_sizes: height == 0 skip ---

    #[test]
    fn test_parse_image_sizes_skips_zero_height() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("name", "bad_h").unwrap();
        s1.set("width", 200u32).unwrap();
        s1.set("height", 0u32).unwrap();
        tbl.set(1, s1).unwrap();
        let sizes = parse_image_sizes(&tbl);
        assert!(sizes.is_empty());
    }

    // --- parse_image_sizes: default fit (unknown string) ---

    #[test]
    fn test_parse_image_sizes_unknown_fit_defaults_to_cover() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s = lua.create_table().unwrap();
        s.set("name", "banner").unwrap();
        s.set("width", 1200u32).unwrap();
        s.set("height", 400u32).unwrap();
        s.set("fit", "stretch").unwrap(); // unknown — defaults to Cover
        tbl.set(1, s).unwrap();
        let sizes = parse_image_sizes(&tbl);
        assert_eq!(sizes.len(), 1);
        assert!(matches!(sizes[0].fit, crate::core::upload::ImageFit::Cover));
    }

    // --- parse_job_definition: 7-field cron (no normalization needed) ---

    #[test]
    fn test_parse_job_definition_7_field_cron() {
        // 7-field cron (already has seconds) should NOT get "0 " prepended
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("handler", "jobs.hourly.run").unwrap();
        tbl.set("schedule", "0 0 * * * * *").unwrap();
        let job = parse_job_definition("hourly", &tbl).unwrap();
        assert_eq!(job.schedule.as_deref(), Some("0 0 * * * * *"));
    }

    // --- parse_auth_strategies: strategy missing name or authenticate is skipped ---

    #[test]
    fn test_parse_auth_strategies_incomplete_strategy_skipped() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        let strats = lua.create_table().unwrap();

        // Strategy with only name, missing authenticate — should be skipped
        let s1 = lua.create_table().unwrap();
        s1.set("name", "incomplete").unwrap();
        strats.set(1, s1).unwrap();

        // Valid strategy
        let s2 = lua.create_table().unwrap();
        s2.set("name", "oauth").unwrap();
        s2.set("authenticate", "hooks.auth.oauth").unwrap();
        strats.set(2, s2).unwrap();

        auth_tbl.set("strategies", strats).unwrap();
        tbl.set("auth", auth_tbl).unwrap();
        let auth = parse_collection_auth(&tbl).unwrap();
        // Only the complete strategy should be included
        assert_eq!(auth.strategies.len(), 1);
        assert_eq!(auth.strategies[0].name, "oauth");
    }

    // --- parse_indexes: missing fields key skipped ---

    #[test]
    fn test_parse_indexes_missing_fields_key_skipped() {
        let lua = Lua::new();
        let config = lua.create_table().unwrap();
        let indexes_tbl = lua.create_table().unwrap();
        let idx = lua.create_table().unwrap();
        // No "fields" key — should be skipped
        idx.set("unique", true).unwrap();
        indexes_tbl.set(1, idx).unwrap();
        config.set("indexes", indexes_tbl).unwrap();
        let indexes = parse_indexes(&config);
        assert!(indexes.is_empty());
    }

    // --- parse_fields: field missing name returns error ---

    #[test]
    fn test_parse_fields_missing_name_returns_error() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        // No "name" set
        field.set("type", "text").unwrap();
        fields_tbl.set(1, field).unwrap();
        let result = parse_fields(&fields_tbl);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing 'name'"));
    }

    // --- parse_fields: min_rows, max_rows, min_length, max_length, min_date, max_date ---

    #[test]
    fn test_parse_fields_array_min_max_rows() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "images").unwrap();
        field.set("type", "array").unwrap();
        field.set("min_rows", 1usize).unwrap();
        field.set("max_rows", 10usize).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].min_rows, Some(1));
        assert_eq!(fields[0].max_rows, Some(10));
    }

    #[test]
    fn test_parse_fields_text_min_max_length() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "slug").unwrap();
        field.set("type", "text").unwrap();
        field.set("min_length", 3usize).unwrap();
        field.set("max_length", 64usize).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].min_length, Some(3));
        assert_eq!(fields[0].max_length, Some(64));
    }

    #[test]
    fn test_parse_fields_date_min_max_date() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "birth_date").unwrap();
        field.set("type", "date").unwrap();
        field.set("min_date", "1900-01-01").unwrap();
        field.set("max_date", "2100-12-31").unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].min_date.as_deref(), Some("1900-01-01"));
        assert_eq!(fields[0].max_date.as_deref(), Some("2100-12-31"));
    }

    // --- parse_fields: admin table with labels, features, nodes ---

    #[test]
    fn test_parse_fields_admin_labels_features_nodes() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "body").unwrap();
        field.set("type", "richtext").unwrap();
        let admin_tbl = lua.create_table().unwrap();
        let labels_tbl = lua.create_table().unwrap();
        labels_tbl.set("singular", "Item").unwrap();
        labels_tbl.set("plural", "Items").unwrap();
        admin_tbl.set("labels", labels_tbl).unwrap();
        let features = lua.create_table().unwrap();
        features.set(1, "bold").unwrap();
        features.set(2, "italic").unwrap();
        admin_tbl.set("features", features).unwrap();
        let nodes = lua.create_table().unwrap();
        nodes.set(1, "paragraph").unwrap();
        admin_tbl.set("nodes", nodes).unwrap();
        admin_tbl.set("format", "lexical").unwrap();
        admin_tbl.set("language", "en").unwrap();
        admin_tbl.set("rows", 5u32).unwrap();
        field.set("admin", admin_tbl).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let admin = &fields[0].admin;
        assert!(admin.labels_singular.is_some());
        assert!(admin.labels_plural.is_some());
        assert_eq!(admin.features, vec!["bold", "italic"]);
        assert_eq!(admin.nodes, vec!["paragraph"]);
        assert_eq!(admin.richtext_format.as_deref(), Some("lexical"));
        assert_eq!(admin.language.as_deref(), Some("en"));
        assert_eq!(admin.rows, Some(5));
    }

    // --- parse_field_hooks: all hook event lists ---

    #[test]
    fn test_parse_field_hooks_all_events() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "title").unwrap();
        field.set("type", "text").unwrap();
        let hooks_tbl = lua.create_table().unwrap();
        let bv = lua.create_table().unwrap();
        bv.set(1, "hooks.validate_title").unwrap();
        hooks_tbl.set("before_validate", bv).unwrap();
        let bc = lua.create_table().unwrap();
        bc.set(1, "hooks.transform_title").unwrap();
        hooks_tbl.set("before_change", bc).unwrap();
        let ac = lua.create_table().unwrap();
        ac.set(1, "hooks.after_title_change").unwrap();
        hooks_tbl.set("after_change", ac).unwrap();
        let ar = lua.create_table().unwrap();
        ar.set(1, "hooks.format_title").unwrap();
        hooks_tbl.set("after_read", ar).unwrap();
        field.set("hooks", hooks_tbl).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&fields_tbl).unwrap();
        let hooks = &fields[0].hooks;
        assert_eq!(hooks.before_validate, vec!["hooks.validate_title"]);
        assert_eq!(hooks.before_change, vec!["hooks.transform_title"]);
        assert_eq!(hooks.after_change, vec!["hooks.after_title_change"]);
        assert_eq!(hooks.after_read, vec!["hooks.format_title"]);
    }

    // --- parse_hooks (collection-level): before_broadcast ---

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

    // --- max_field_nesting: deep nesting via blocks and tabs ---

    #[test]
    fn test_max_field_nesting_via_blocks() {
        use crate::core::field::{BlockDefinition, FieldDefinition, FieldType};
        let inner = FieldDefinition {
            name: "text".to_string(),
            field_type: FieldType::Text,
            ..Default::default()
        };
        let block = BlockDefinition {
            block_type: "para".to_string(),
            fields: vec![inner],
            label: None,
            label_field: None,
            group: None,
            image_url: None,
        };
        let outer = FieldDefinition {
            name: "content".to_string(),
            field_type: FieldType::Blocks,
            blocks: vec![block],
            ..Default::default()
        };
        // depth at current=0: sub current+1=1, block fields at depth 1+1=2 → max is 2
        let depth = max_field_nesting(&[outer], 0);
        assert_eq!(depth, 2);
    }

    #[test]
    fn test_max_field_nesting_via_tabs() {
        use crate::core::field::{FieldDefinition, FieldTab, FieldType};
        let inner = FieldDefinition {
            name: "bio".to_string(),
            field_type: FieldType::Textarea,
            ..Default::default()
        };
        let tab = FieldTab {
            label: "General".to_string(),
            description: None,
            fields: vec![inner],
        };
        let outer = FieldDefinition {
            name: "tabs".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![tab],
            ..Default::default()
        };
        let depth = max_field_nesting(&[outer], 0);
        assert_eq!(depth, 2);
    }

    // --- warn_deep_nesting: exercises the > ADMIN_MAX_FIELD_DEPTH branch ---

    #[test]
    fn test_warn_deep_nesting_triggers_for_deep_fields() {
        use crate::core::field::{FieldDefinition, FieldType};

        // Build a chain of 6 nested groups (exceeds ADMIN_MAX_FIELD_DEPTH = 5)
        fn nest(depth: usize) -> FieldDefinition {
            if depth == 0 {
                FieldDefinition {
                    name: "leaf".to_string(),
                    field_type: FieldType::Text,
                    ..Default::default()
                }
            } else {
                FieldDefinition {
                    name: format!("level_{}", depth),
                    field_type: FieldType::Group,
                    fields: vec![nest(depth - 1)],
                    ..Default::default()
                }
            }
        }

        let deep_field = nest(6);
        // warn_deep_nesting is a tracing::warn! — just ensure it doesn't panic
        warn_deep_nesting("Collection", "test", &[deep_field]);
    }

    // --- parse_collection_definition: auth email injection when email field already exists ---

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
        // Email should only appear once (not injected since it already exists)
        let email_count = def.fields.iter().filter(|f| f.name == "email").count();
        assert_eq!(email_count, 1);
    }

    // --- parse_collection_upload: table with no mime_types ---

    #[test]
    fn test_parse_collection_upload_table_no_mime_types() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        // No mime_types key — should default to empty vec
        tbl.set("upload", upload_tbl).unwrap();
        let upload = parse_collection_upload(&tbl).unwrap();
        assert!(upload.mime_types.is_empty());
        assert!(upload.max_file_size.is_none());
    }

    // --- parse_global_definition: labels parsing ---

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
