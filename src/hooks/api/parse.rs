//! Parsing functions for collection/global Lua definitions into Rust types.

use anyhow::Result;
use mlua::{Lua, Table, Value};

use crate::core::{
    field::{FieldType, FieldDefinition, FieldAccess, FieldAdmin, FieldHooks, SelectOption, LocalizedString},
    collection::{AuthStrategy, CollectionAccess, CollectionAuth, CollectionDefinition, GlobalDefinition, CollectionLabels, CollectionAdmin, CollectionHooks, LiveSetting, VersionsConfig},
    upload::{CollectionUpload, ImageSize, ImageFit, FormatOptions, FormatQuality},
};

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

    // If auth enabled and no email field defined, inject one at index 0
    if let Some(ref a) = auth {
        if a.enabled && !fields.iter().any(|f| f.name == "email") {
            fields.insert(0, FieldDefinition {
                name: "email".to_string(),
                field_type: FieldType::Email,
                required: true,
                unique: true,
                validate: None,
                default_value: None,
                options: Vec::new(),
                admin: FieldAdmin {
                    placeholder: Some(LocalizedString::Plain("user@example.com".to_string())),
                    ..Default::default()
                },
                hooks: FieldHooks::default(),
                access: FieldAccess::default(),
                relationship: None,
                fields: Vec::new(),
                blocks: Vec::new(),
                localized: false,
                picker_appearance: None,
            });
        }
    }

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
        live,
        versions,
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

    let hooks = if let Ok(hooks_tbl) = get_table(config, "hooks") {
        parse_hooks(&hooks_tbl)?
    } else {
        CollectionHooks::default()
    };

    let access = parse_access_config(config);
    let live = parse_live_setting(config);

    Ok(GlobalDefinition {
        slug: slug.to_string(),
        labels,
        fields,
        hooks,
        access,
        live,
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

            let max_file_size = tbl.get::<Option<u64>>("max_file_size").ok().flatten();

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
        FormatQuality { quality }
    });

    let avif = get_table(&fo_tbl, "avif").ok().map(|t| {
        let quality = t.get::<u8>("quality").unwrap_or(60);
        FormatQuality { quality }
    });

    FormatOptions { webp, avif }
}

/// Helper to create a hidden text field definition.
fn hidden_text_field(name: &str) -> FieldDefinition {
    FieldDefinition {
        name: name.to_string(),
        field_type: FieldType::Text,
        required: false,
        unique: false,
        validate: None,
        default_value: None,
        options: Vec::new(),
        admin: FieldAdmin { hidden: true, ..Default::default() },
        hooks: FieldHooks::default(),
        access: FieldAccess::default(),
        relationship: None,
        fields: Vec::new(),
        blocks: Vec::new(),
        localized: false,
        picker_appearance: None,
    }
}

/// Helper to create a hidden number field definition.
fn hidden_number_field(name: &str) -> FieldDefinition {
    FieldDefinition {
        name: name.to_string(),
        field_type: FieldType::Number,
        required: false,
        unique: false,
        validate: None,
        default_value: None,
        options: Vec::new(),
        admin: FieldAdmin { hidden: true, ..Default::default() },
        hooks: FieldHooks::default(),
        access: FieldAccess::default(),
        relationship: None,
        fields: Vec::new(),
        blocks: Vec::new(),
        localized: false,
        picker_appearance: None,
    }
}

/// Auto-inject upload metadata fields at position 0 (before user fields).
/// Generates typed columns for each image size instead of a JSON blob.
fn inject_upload_fields(fields: &mut Vec<FieldDefinition>, upload: &CollectionUpload) {
    let mut upload_fields = vec![
        FieldDefinition {
            name: "filename".to_string(),
            field_type: FieldType::Text,
            required: true,
            unique: false,
            validate: None,
            default_value: None,
            options: Vec::new(),
            admin: FieldAdmin { readonly: true, ..Default::default() },
            hooks: FieldHooks::default(),
            access: FieldAccess::default(),
            relationship: None,
            fields: Vec::new(),
            blocks: Vec::new(),
            localized: false,
            picker_appearance: None,
        },
        hidden_text_field("mime_type"),
        hidden_number_field("filesize"),
        hidden_number_field("width"),
        hidden_number_field("height"),
        hidden_text_field("url"),
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

fn parse_fields(fields_tbl: &Table) -> Result<Vec<FieldDefinition>> {
    let mut fields = Vec::new();

    for pair in fields_tbl.clone().sequence_values::<Table>() {
        let field_tbl = pair?;
        let name: String = get_string_val(&field_tbl, "name")
            .map_err(|_| anyhow::anyhow!("Field missing 'name'"))?;
        let type_str: String = get_string_val(&field_tbl, "type").unwrap_or_else(|_| "text".to_string());
        let field_type = FieldType::from_str(&type_str);

        let required = get_bool(&field_tbl, "required", false);
        let unique = get_bool(&field_tbl, "unique", false);
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
            FieldAdmin {
                label: get_localized_string(&admin_tbl, "label"),
                placeholder: get_localized_string(&admin_tbl, "placeholder"),
                description: get_localized_string(&admin_tbl, "description"),
                hidden: get_bool(&admin_tbl, "hidden", false),
                readonly: get_bool(&admin_tbl, "readonly", false),
                width: get_string(&admin_tbl, "width"),
                collapsed: get_bool(&admin_tbl, "collapsed", false),
            }
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
        let relationship = if field_type == FieldType::Relationship {
            if let Ok(rel_tbl) = get_table(&field_tbl, "relationship") {
                let collection = get_string(&rel_tbl, "collection").unwrap_or_default();
                let has_many = get_bool(&rel_tbl, "has_many", false);
                let max_depth = rel_tbl.get::<Option<i32>>("max_depth").ok().flatten();
                Some(crate::core::field::RelationshipConfig { collection, has_many, max_depth })
            } else {
                // Legacy flat syntax: relation_to + has_many on the field itself
                get_string(&field_tbl, "relation_to").map(|collection| {
                    let has_many = get_bool(&field_tbl, "has_many", false);
                    crate::core::field::RelationshipConfig { collection, has_many, max_depth: None }
                })
            }
        } else if field_type == FieldType::Upload {
            // Upload: auto-create has-one relationship config from relation_to
            if let Ok(rel_tbl) = get_table(&field_tbl, "relationship") {
                let collection = get_string(&rel_tbl, "collection").unwrap_or_default();
                let max_depth = rel_tbl.get::<Option<i32>>("max_depth").ok().flatten();
                Some(crate::core::field::RelationshipConfig { collection, has_many: false, max_depth })
            } else {
                get_string(&field_tbl, "relation_to").map(|collection| {
                    crate::core::field::RelationshipConfig { collection, has_many: false, max_depth: None }
                })
            }
        } else {
            None
        };

        // Parse sub-fields for Array and Group types (recursive)
        let sub_fields = if field_type == FieldType::Array || field_type == FieldType::Group {
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

        fields.push(FieldDefinition {
            name,
            field_type,
            required,
            unique,
            validate,
            default_value,
            options,
            admin,
            hooks,
            access,
            relationship,
            fields: sub_fields,
            blocks: block_defs,
            localized,
            picker_appearance,
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
        let fields = if let Ok(fields_tbl) = get_table(&block_tbl, "fields") {
            parse_fields(&fields_tbl)?
        } else {
            Vec::new()
        };
        blocks.push(crate::core::field::BlockDefinition {
            block_type,
            fields,
            label,
        });
    }
    Ok(blocks)
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

fn get_string_val(tbl: &Table, key: &str) -> mlua::Result<String> {
    tbl.get(key)
}

/// Parse a Lua table into a `JobDefinition`.
pub fn parse_job_definition(slug: &str, config: &Table) -> Result<crate::core::job::JobDefinition> {
    use crate::core::job::{JobDefinition, JobLabels};

    let handler = get_string(config, "handler")
        .ok_or_else(|| anyhow::anyhow!("Job '{}' missing required 'handler' field", slug))?;

    let schedule = get_string(config, "schedule");
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
