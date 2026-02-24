//! Registers the `crap.*` Lua API namespace (collections, globals, hooks, log, util).

use anyhow::{Context, Result};
use mlua::{Lua, Table, Value, Function};
use std::path::Path;

use crate::config::CrapConfig;
use crate::core::{
    SharedRegistry,
    field::{FieldType, FieldDefinition, FieldAccess, FieldAdmin, FieldHooks, SelectOption, LocalizedString},
    collection::{AuthStrategy, CollectionAccess, CollectionAuth, CollectionDefinition, GlobalDefinition, CollectionLabels, CollectionAdmin, CollectionHooks, LiveSetting},
    upload::{CollectionUpload, ImageSize, ImageFit, FormatOptions, FormatQuality},
};

/// Register the `crap` global table with sub-tables for collections, globals, log, util,
/// auth, env, http, config.
pub fn register_api(lua: &Lua, registry: SharedRegistry, _config_dir: &Path, config: &CrapConfig) -> Result<()> {
    let crap = lua.create_table().context("Failed to create crap table")?;

    // crap.collections
    let collections_table = lua.create_table()?;
    let reg_clone = registry.clone();
    let define_collection = lua.create_function(move |lua, (slug, config): (String, Table)| {
        let def = parse_collection_definition(lua, &slug, &config)
            .map_err(|e| mlua::Error::RuntimeError(format!(
                "Failed to parse collection '{}': {}", slug, e
            )))?;
        let mut reg = reg_clone.write()
            .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock poisoned: {}", e)))?;
        reg.register_collection(def);
        Ok(())
    })?;
    collections_table.set("define", define_collection)?;
    crap.set("collections", collections_table)?;

    // crap.globals
    let globals_table = lua.create_table()?;
    let reg_clone = registry.clone();
    let define_global = lua.create_function(move |lua, (slug, config): (String, Table)| {
        let def = parse_global_definition(lua, &slug, &config)
            .map_err(|e| mlua::Error::RuntimeError(format!(
                "Failed to parse global '{}': {}", slug, e
            )))?;
        let mut reg = reg_clone.write()
            .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock poisoned: {}", e)))?;
        reg.register_global(def);
        Ok(())
    })?;
    globals_table.set("define", define_global)?;
    crap.set("globals", globals_table)?;

    // crap.log
    let log_table = lua.create_table()?;
    let log_info = lua.create_function(|_, msg: String| {
        tracing::info!("[lua] {}", msg);
        Ok(())
    })?;
    let log_warn = lua.create_function(|_, msg: String| {
        tracing::warn!("[lua] {}", msg);
        Ok(())
    })?;
    let log_error = lua.create_function(|_, msg: String| {
        tracing::error!("[lua] {}", msg);
        Ok(())
    })?;
    log_table.set("info", log_info)?;
    log_table.set("warn", log_warn)?;
    log_table.set("error", log_error)?;
    crap.set("log", log_table)?;

    // crap.util
    let util_table = lua.create_table()?;

    let slugify_fn = lua.create_function(|_, s: String| {
        Ok(slugify(&s))
    })?;
    util_table.set("slugify", slugify_fn)?;

    let nanoid_fn = lua.create_function(|_, ()| {
        Ok(nanoid::nanoid!())
    })?;
    util_table.set("nanoid", nanoid_fn)?;

    let json_encode_fn: Function = lua.create_function(|lua, value: Value| {
        let json_value = lua_to_json(lua, &value)?;
        serde_json::to_string(&json_value)
            .map_err(|e| mlua::Error::RuntimeError(format!("JSON encode error: {}", e)))
    })?;
    util_table.set("json_encode", json_encode_fn)?;

    let json_decode_fn = lua.create_function(|lua, s: String| {
        let value: serde_json::Value = serde_json::from_str(&s)
            .map_err(|e| mlua::Error::RuntimeError(format!("JSON decode error: {}", e)))?;
        json_to_lua(lua, &value)
    })?;
    util_table.set("json_decode", json_decode_fn)?;

    crap.set("util", util_table)?;

    // _crap_event_hooks — Lua-side storage for registered global hooks
    let event_hooks = lua.create_table()?;
    lua.globals().set("_crap_event_hooks", event_hooks)?;

    // crap.hooks
    let hooks_table = lua.create_table()?;

    // crap.hooks.register(event, fn) — append fn to _crap_event_hooks[event]
    let register_fn = lua.create_function(|lua, (event, func): (String, Function)| {
        let globals = lua.globals();
        let event_hooks: Table = globals.get("_crap_event_hooks")?;
        let list: Table = match event_hooks.get::<Value>(event.as_str())? {
            Value::Table(t) => t,
            _ => {
                let t = lua.create_table()?;
                event_hooks.set(event.as_str(), t.clone())?;
                t
            }
        };
        let len = list.raw_len();
        list.set(len + 1, func)?;
        Ok(())
    })?;
    hooks_table.set("register", register_fn)?;

    // crap.hooks.remove(event, fn) — find and remove fn using rawequal
    let remove_fn = lua.create_function(|lua, (event, func): (String, Function)| {
        let globals = lua.globals();
        let event_hooks: Table = globals.get("_crap_event_hooks")?;
        let list: Table = match event_hooks.get::<Value>(event.as_str())? {
            Value::Table(t) => t,
            _ => return Ok(()),
        };
        // Find the index of the matching function using rawequal
        let rawequal: Function = globals.get("rawequal")?;
        let len = list.raw_len();
        let mut remove_idx = None;
        for i in 1..=len {
            let entry: Value = list.raw_get(i)?;
            let eq: bool = rawequal.call((entry, func.clone()))?;
            if eq {
                remove_idx = Some(i);
                break;
            }
        }
        // Remove by shifting elements down
        if let Some(idx) = remove_idx {
            let table_remove: Function = lua.load("table.remove").eval()?;
            table_remove.call::<()>((list, idx))?;
        }
        Ok(())
    })?;
    hooks_table.set("remove", remove_fn)?;

    crap.set("hooks", hooks_table)?;

    // crap.auth — password hashing/verification
    let auth_table = lua.create_table()?;
    let hash_pw_fn = lua.create_function(|_, password: String| {
        crate::core::auth::hash_password(&password)
            .map_err(|e| mlua::Error::RuntimeError(format!("hash_password error: {}", e)))
    })?;
    let verify_pw_fn = lua.create_function(|_, (password, hash): (String, String)| {
        crate::core::auth::verify_password(&password, &hash)
            .map_err(|e| mlua::Error::RuntimeError(format!("verify_password error: {}", e)))
    })?;
    auth_table.set("hash_password", hash_pw_fn)?;
    auth_table.set("verify_password", verify_pw_fn)?;
    crap.set("auth", auth_table)?;

    // crap.env — read-only env var access
    let env_table = lua.create_table()?;
    let env_get_fn = lua.create_function(|_, key: String| -> mlua::Result<Option<String>> {
        match std::env::var(&key) {
            Ok(val) => Ok(Some(val)),
            Err(_) => Ok(None),
        }
    })?;
    env_table.set("get", env_get_fn)?;
    crap.set("env", env_table)?;

    // crap.http — outbound HTTP via ureq (blocking, safe in spawn_blocking context)
    let http_table = lua.create_table()?;
    let http_request_fn = lua.create_function(|lua, opts: Table| -> mlua::Result<Table> {
        let url: String = opts.get("url")?;
        let method: String = opts.get::<Option<String>>("method")?
            .unwrap_or_else(|| "GET".to_string())
            .to_uppercase();
        let timeout: u64 = opts.get::<Option<u64>>("timeout")?.unwrap_or(30);
        let body: Option<String> = opts.get("body")?;

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(timeout))
            .build();

        let mut req = match method.as_str() {
            "GET" => agent.get(&url),
            "POST" => agent.post(&url),
            "PUT" => agent.put(&url),
            "PATCH" => agent.request("PATCH", &url),
            "DELETE" => agent.delete(&url),
            "HEAD" => agent.head(&url),
            _ => return Err(mlua::Error::RuntimeError(
                format!("unsupported HTTP method: {}", method)
            )),
        };

        // Set request headers
        if let Ok(headers_tbl) = opts.get::<Table>("headers") {
            for pair in headers_tbl.pairs::<String, String>() {
                let (k, v) = pair?;
                req = req.set(&k, &v);
            }
        }

        // Send request
        let response = if let Some(body_str) = body {
            req.send_string(&body_str)
        } else {
            req.call()
        };

        let result = lua.create_table()?;
        match response {
            Ok(resp) => {
                result.set("status", resp.status() as i64)?;
                let headers_out = lua.create_table()?;
                for name in resp.headers_names() {
                    if let Some(val) = resp.header(&name) {
                        headers_out.set(name.as_str(), val)?;
                    }
                }
                result.set("headers", headers_out)?;
                let body_str = resp.into_string()
                    .map_err(|e| mlua::Error::RuntimeError(
                        format!("failed to read response body: {}", e)
                    ))?;
                result.set("body", body_str)?;
            }
            Err(ureq::Error::Status(code, resp)) => {
                result.set("status", code as i64)?;
                let headers_out = lua.create_table()?;
                for name in resp.headers_names() {
                    if let Some(val) = resp.header(&name) {
                        headers_out.set(name.as_str(), val)?;
                    }
                }
                result.set("headers", headers_out)?;
                let body_str = resp.into_string().unwrap_or_default();
                result.set("body", body_str)?;
            }
            Err(ureq::Error::Transport(e)) => {
                return Err(mlua::Error::RuntimeError(
                    format!("HTTP transport error: {}", e)
                ));
            }
        }

        Ok(result)
    })?;
    http_table.set("request", http_request_fn)?;
    crap.set("http", http_table)?;

    // crap.config — read-only config access with dot notation
    let config_table = lua.create_table()?;
    // Serialize config to JSON, then to a Lua table stored as _crap_config
    let config_json = serde_json::to_value(config)
        .map_err(|e| anyhow::anyhow!("Failed to serialize config: {}", e))?;
    let config_lua = json_to_lua(lua, &config_json)?;
    lua.globals().set("_crap_config", config_lua)?;

    let config_get_fn = lua.create_function(|lua, key: String| -> mlua::Result<Value> {
        let config_val: Value = lua.globals().get("_crap_config")?;
        let mut current = config_val;
        for part in key.split('.') {
            match current {
                Value::Table(tbl) => {
                    current = tbl.get(part)?;
                }
                _ => return Ok(Value::Nil),
            }
        }
        Ok(current)
    })?;
    config_table.set("get", config_get_fn)?;
    crap.set("config", config_table)?;

    // crap.locale — locale configuration access
    let locale_table = lua.create_table()?;
    {
        let default_locale = config.locale.default_locale.clone();
        let get_default_fn = lua.create_function(move |_, ()| -> mlua::Result<String> {
            Ok(default_locale.clone())
        })?;
        locale_table.set("get_default", get_default_fn)?;

        let locales = config.locale.locales.clone();
        let get_all_fn = lua.create_function(move |lua, ()| -> mlua::Result<Table> {
            let tbl = lua.create_table()?;
            for (i, l) in locales.iter().enumerate() {
                tbl.set(i + 1, l.as_str())?;
            }
            Ok(tbl)
        })?;
        locale_table.set("get_all", get_all_fn)?;

        let enabled = config.locale.is_enabled();
        let is_enabled_fn = lua.create_function(move |_, ()| -> mlua::Result<bool> {
            Ok(enabled)
        })?;
        locale_table.set("is_enabled", is_enabled_fn)?;
    }
    crap.set("locale", locale_table)?;

    // crap.email — outbound email sending via SMTP
    let email_table = lua.create_table()?;
    let email_config = config.email.clone();
    let email_send_fn = lua.create_function(move |_, opts: Table| -> mlua::Result<bool> {
        let to: String = opts.get("to")?;
        let subject: String = opts.get("subject")?;
        let html: String = opts.get("html")?;
        let text: Option<String> = opts.get("text")?;

        crate::core::email::send_email(
            &email_config,
            &to,
            &subject,
            &html,
            text.as_deref(),
        ).map_err(|e| mlua::Error::RuntimeError(format!("email send error: {}", e)))?;

        Ok(true)
    })?;
    email_table.set("send", email_send_fn)?;
    crap.set("email", email_table)?;

    lua.globals().set("crap", crap)?;
    Ok(())
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

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
            for pair in t.pairs::<String, String>() {
                if let Ok((k, v)) = pair {
                    map.insert(k, v);
                }
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

fn parse_collection_definition(_lua: &Lua, slug: &str, config: &Table) -> Result<CollectionDefinition> {
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
    })
}

fn parse_global_definition(_lua: &Lua, slug: &str, config: &Table) -> Result<GlobalDefinition> {
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
/// - Absent / `true` → `None` (broadcast all events)
/// - `false` → `Some(LiveSetting::Disabled)`
/// - String → `Some(LiveSetting::Function(ref))`
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
    for entry in strategies_tbl.sequence_values::<Table>() {
        if let Ok(strat_tbl) = entry {
            if let (Some(name), Some(authenticate)) = (
                get_string(&strat_tbl, "name"),
                get_string(&strat_tbl, "authenticate"),
            ) {
                strategies.push(AuthStrategy { name, authenticate });
            }
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
    for entry in tbl.sequence_values::<Table>() {
        if let Ok(size_tbl) = entry {
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

/// Convert a Lua value to a serde_json::Value.
pub fn lua_to_json(_lua: &Lua, value: &Value) -> mlua::Result<serde_json::Value> {
    match value {
        Value::Nil => Ok(serde_json::Value::Null),
        Value::Boolean(b) => Ok(serde_json::Value::Bool(*b)),
        Value::Integer(i) => Ok(serde_json::Value::Number((*i).into())),
        Value::Number(n) => {
            serde_json::Number::from_f64(*n)
                .map(serde_json::Value::Number)
                .ok_or_else(|| mlua::Error::RuntimeError("Invalid float value".into()))
        }
        Value::String(s) => Ok(serde_json::Value::String(s.to_str()?.to_string())),
        Value::Table(t) => {
            let len = t.raw_len();
            if len > 0 {
                let mut arr = Vec::new();
                for i in 1..=len {
                    let v: Value = t.raw_get(i)?;
                    arr.push(lua_to_json(_lua, &v)?);
                }
                Ok(serde_json::Value::Array(arr))
            } else {
                let mut map = serde_json::Map::new();
                for pair in t.clone().pairs::<String, Value>() {
                    let (k, v) = pair?;
                    map.insert(k, lua_to_json(_lua, &v)?);
                }
                Ok(serde_json::Value::Object(map))
            }
        }
        _ => Ok(serde_json::Value::Null),
    }
}

/// Convert a serde_json::Value to a Lua value.
pub fn json_to_lua(lua: &Lua, value: &serde_json::Value) -> mlua::Result<Value> {
    match value {
        serde_json::Value::Null => Ok(Value::Nil),
        serde_json::Value::Bool(b) => Ok(Value::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Value::Number(f))
            } else {
                Ok(Value::Nil)
            }
        }
        serde_json::Value::String(s) => {
            Ok(Value::String(lua.create_string(s)?))
        }
        serde_json::Value::Array(arr) => {
            let tbl = lua.create_table()?;
            for (i, v) in arr.iter().enumerate() {
                tbl.set(i + 1, json_to_lua(lua, v)?)?;
            }
            Ok(Value::Table(tbl))
        }
        serde_json::Value::Object(map) => {
            let tbl = lua.create_table()?;
            for (k, v) in map {
                tbl.set(k.as_str(), json_to_lua(lua, v)?)?;
            }
            Ok(Value::Table(tbl))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World"), "hello-world");
    }

    #[test]
    fn slugify_special_chars() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
    }

    #[test]
    fn slugify_multiple_spaces() {
        assert_eq!(slugify("hello   world"), "hello-world");
    }

    #[test]
    fn slugify_leading_trailing() {
        assert_eq!(slugify("  hello  "), "hello");
    }

    #[test]
    fn slugify_already_clean() {
        assert_eq!(slugify("hello-world"), "hello-world");
    }

    #[test]
    fn slugify_empty() {
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn slugify_unicode() {
        assert_eq!(slugify("Caf\u{00e9} Latt\u{00e9}"), "caf\u{00e9}-latt\u{00e9}");
    }
}
