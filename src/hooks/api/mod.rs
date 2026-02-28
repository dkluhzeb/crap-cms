//! Registers the `crap.*` Lua API namespace (collections, globals, hooks, log, util,
//! crypto, schema).

pub mod parse;
mod util;
mod schema;
mod crypto;
mod http;
mod email;
mod jobs;
mod config;

use anyhow::{Context, Result};
use mlua::{Lua, Table, Value, Function};
use std::path::Path;

use crate::config::CrapConfig;
use crate::core::SharedRegistry;

use parse::{parse_collection_definition, parse_global_definition};

/// Register the `crap` global table with sub-tables for collections, globals, log, util,
/// auth, env, http, config.
pub fn register_api(lua: &Lua, registry: SharedRegistry, _config_dir: &Path, config: &CrapConfig) -> Result<()> {
    let crap = lua.create_table().context("Failed to create crap table")?;

    register_collections(lua, &crap, registry.clone())?;
    register_globals(lua, &crap, registry.clone())?;
    register_log(lua, &crap)?;
    util::register_util(lua, &crap)?;
    crypto::register_crypto(lua, &crap, &config.auth.secret)?;
    schema::register_schema(lua, &crap, registry.clone())?;
    register_hooks(lua, &crap)?;
    register_auth(lua, &crap)?;
    register_env(lua, &crap)?;
    http::register_http(lua, &crap)?;
    config::register_config(lua, &crap, config)?;
    config::register_locale(lua, &crap, config)?;
    jobs::register_jobs(lua, &crap, registry.clone())?;
    email::register_email(lua, &crap, config)?;

    lua.globals().set("crap", crap)?;

    // Load pure Lua helpers onto crap.util (after crap global is set)
    util::load_lua_helpers(lua)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Per-namespace registration helpers (kept in mod.rs — small functions)
// ---------------------------------------------------------------------------

/// Register `crap.collections` — define, config.get, config.list.
fn register_collections(lua: &Lua, crap: &Table, registry: SharedRegistry) -> Result<()> {
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

    let reg_clone = registry.clone();
    let get_collection = lua.create_function(move |lua, slug: String| -> mlua::Result<Value> {
        let reg = reg_clone.read()
            .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock poisoned: {}", e)))?;
        match reg.get_collection(&slug) {
            Some(def) => Ok(Value::Table(collection_config_to_lua(lua, def)?)),
            None => Ok(Value::Nil),
        }
    })?;
    let collections_config_table = lua.create_table()?;
    collections_config_table.set("get", get_collection)?;

    let reg_clone = registry.clone();
    let list_collections = lua.create_function(move |lua, ()| -> mlua::Result<Table> {
        let reg = reg_clone.read()
            .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock poisoned: {}", e)))?;
        let map = lua.create_table()?;
        for (slug, def) in reg.collections.iter() {
            map.set(slug.as_str(), collection_config_to_lua(lua, def)?)?;
        }
        Ok(map)
    })?;
    collections_config_table.set("list", list_collections)?;
    collections_table.set("config", collections_config_table)?;

    crap.set("collections", collections_table)?;
    Ok(())
}

/// Register `crap.globals` — define, config.get, config.list.
fn register_globals(lua: &Lua, crap: &Table, registry: SharedRegistry) -> Result<()> {
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

    let reg_clone = registry.clone();
    let get_global = lua.create_function(move |lua, slug: String| -> mlua::Result<Value> {
        let reg = reg_clone.read()
            .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock poisoned: {}", e)))?;
        match reg.get_global(&slug) {
            Some(def) => Ok(Value::Table(global_config_to_lua(lua, def)?)),
            None => Ok(Value::Nil),
        }
    })?;
    let globals_config_table = lua.create_table()?;
    globals_config_table.set("get", get_global)?;

    let reg_clone = registry.clone();
    let list_globals = lua.create_function(move |lua, ()| -> mlua::Result<Table> {
        let reg = reg_clone.read()
            .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock poisoned: {}", e)))?;
        let map = lua.create_table()?;
        for (slug, def) in reg.globals.iter() {
            map.set(slug.as_str(), global_config_to_lua(lua, def)?)?;
        }
        Ok(map)
    })?;
    globals_config_table.set("list", list_globals)?;
    globals_table.set("config", globals_config_table)?;

    crap.set("globals", globals_table)?;
    Ok(())
}

/// Register `crap.log` — info, warn, error.
fn register_log(lua: &Lua, crap: &Table) -> Result<()> {
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
    Ok(())
}

/// Register `crap.hooks` — register/remove global event hooks, plus `_crap_event_hooks` storage.
fn register_hooks(lua: &Lua, crap: &Table) -> Result<()> {
    // _crap_event_hooks — Lua-side storage for registered global hooks
    let event_hooks = lua.create_table()?;
    lua.globals().set("_crap_event_hooks", event_hooks)?;

    let hooks_table = lua.create_table()?;

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

    let remove_fn = lua.create_function(|lua, (event, func): (String, Function)| {
        let globals = lua.globals();
        let event_hooks: Table = globals.get("_crap_event_hooks")?;
        let list: Table = match event_hooks.get::<Value>(event.as_str())? {
            Value::Table(t) => t,
            _ => return Ok(()),
        };
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
        if let Some(idx) = remove_idx {
            let table_remove: Function = lua.load("table.remove").eval()?;
            table_remove.call::<()>((list, idx))?;
        }
        Ok(())
    })?;
    hooks_table.set("remove", remove_fn)?;

    crap.set("hooks", hooks_table)?;
    Ok(())
}

/// Register `crap.auth` — hash_password, verify_password.
fn register_auth(lua: &Lua, crap: &Table) -> Result<()> {
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
    Ok(())
}

/// Register `crap.env` — read-only env var access.
fn register_env(lua: &Lua, crap: &Table) -> Result<()> {
    let env_table = lua.create_table()?;
    let env_get_fn = lua.create_function(|_, key: String| -> mlua::Result<Option<String>> {
        match std::env::var(&key) {
            Ok(val) => Ok(Some(val)),
            Err(_) => Ok(None),
        }
    })?;
    env_table.set("get", env_get_fn)?;
    crap.set("env", env_table)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Convert a LocalizedString to a Lua value (string or locale table).
fn localized_string_to_lua(lua: &Lua, ls: &crate::core::field::LocalizedString) -> mlua::Result<Value> {
    match ls {
        crate::core::field::LocalizedString::Plain(s) => {
            Ok(Value::String(lua.create_string(s)?))
        }
        crate::core::field::LocalizedString::Localized(map) => {
            let tbl = lua.create_table()?;
            for (k, v) in map {
                tbl.set(k.as_str(), v.as_str())?;
            }
            Ok(Value::Table(tbl))
        }
    }
}

/// Convert a CollectionDefinition to a full Lua table compatible with parse_collection_definition().
/// Unlike collection_def_to_lua_table (used by crap.schema), this produces a round-trip compatible
/// table that can be passed back to crap.collections.define().
fn collection_config_to_lua(lua: &Lua, def: &crate::core::CollectionDefinition) -> mlua::Result<Table> {
    let tbl = lua.create_table()?;

    // labels
    let labels = lua.create_table()?;
    if let Some(ref s) = def.labels.singular {
        labels.set("singular", localized_string_to_lua(lua, s)?)?;
    }
    if let Some(ref s) = def.labels.plural {
        labels.set("plural", localized_string_to_lua(lua, s)?)?;
    }
    tbl.set("labels", labels)?;

    tbl.set("timestamps", def.timestamps)?;

    // admin
    let admin = lua.create_table()?;
    if let Some(ref s) = def.admin.use_as_title {
        admin.set("use_as_title", s.as_str())?;
    }
    if let Some(ref s) = def.admin.default_sort {
        admin.set("default_sort", s.as_str())?;
    }
    if def.admin.hidden {
        admin.set("hidden", true)?;
    }
    if !def.admin.list_searchable_fields.is_empty() {
        let lsf = lua.create_table()?;
        for (i, f) in def.admin.list_searchable_fields.iter().enumerate() {
            lsf.set(i + 1, f.as_str())?;
        }
        admin.set("list_searchable_fields", lsf)?;
    }
    tbl.set("admin", admin)?;

    // fields
    let fields_arr = lua.create_table()?;
    for (i, f) in def.fields.iter().enumerate() {
        fields_arr.set(i + 1, field_config_to_lua(lua, f)?)?;
    }
    tbl.set("fields", fields_arr)?;

    // hooks
    let hooks = collection_hooks_to_lua(lua, &def.hooks)?;
    tbl.set("hooks", hooks)?;

    // access
    let access = lua.create_table()?;
    if let Some(ref s) = def.access.read { access.set("read", s.as_str())?; }
    if let Some(ref s) = def.access.create { access.set("create", s.as_str())?; }
    if let Some(ref s) = def.access.update { access.set("update", s.as_str())?; }
    if let Some(ref s) = def.access.delete { access.set("delete", s.as_str())?; }
    tbl.set("access", access)?;

    // auth
    if let Some(ref auth) = def.auth {
        if auth.enabled {
            if auth.strategies.is_empty()
                && !auth.disable_local
                && !auth.verify_email
                && auth.forgot_password
                && auth.token_expiry == 7200
            {
                tbl.set("auth", true)?;
            } else {
                let auth_tbl = lua.create_table()?;
                auth_tbl.set("token_expiry", auth.token_expiry)?;
                if auth.disable_local {
                    auth_tbl.set("disable_local", true)?;
                }
                if auth.verify_email {
                    auth_tbl.set("verify_email", true)?;
                }
                if !auth.forgot_password {
                    auth_tbl.set("forgot_password", false)?;
                }
                if !auth.strategies.is_empty() {
                    let strats = lua.create_table()?;
                    for (i, s) in auth.strategies.iter().enumerate() {
                        let st = lua.create_table()?;
                        st.set("name", s.name.as_str())?;
                        st.set("authenticate", s.authenticate.as_str())?;
                        strats.set(i + 1, st)?;
                    }
                    auth_tbl.set("strategies", strats)?;
                }
                tbl.set("auth", auth_tbl)?;
            }
        }
    }

    // upload
    if let Some(ref upload) = def.upload {
        if upload.enabled {
            if upload.mime_types.is_empty()
                && upload.max_file_size.is_none()
                && upload.image_sizes.is_empty()
                && upload.admin_thumbnail.is_none()
                && upload.format_options.webp.is_none()
                && upload.format_options.avif.is_none()
            {
                tbl.set("upload", true)?;
            } else {
                let u = lua.create_table()?;
                if !upload.mime_types.is_empty() {
                    let mt = lua.create_table()?;
                    for (i, m) in upload.mime_types.iter().enumerate() {
                        mt.set(i + 1, m.as_str())?;
                    }
                    u.set("mime_types", mt)?;
                }
                if let Some(max) = upload.max_file_size {
                    u.set("max_file_size", max)?;
                }
                if !upload.image_sizes.is_empty() {
                    let sizes = lua.create_table()?;
                    for (i, s) in upload.image_sizes.iter().enumerate() {
                        let st = lua.create_table()?;
                        st.set("name", s.name.as_str())?;
                        st.set("width", s.width)?;
                        st.set("height", s.height)?;
                        let fit_str = match s.fit {
                            crate::core::upload::ImageFit::Cover => "cover",
                            crate::core::upload::ImageFit::Contain => "contain",
                            crate::core::upload::ImageFit::Inside => "inside",
                            crate::core::upload::ImageFit::Fill => "fill",
                        };
                        st.set("fit", fit_str)?;
                        sizes.set(i + 1, st)?;
                    }
                    u.set("image_sizes", sizes)?;
                }
                if let Some(ref thumb) = upload.admin_thumbnail {
                    u.set("admin_thumbnail", thumb.as_str())?;
                }
                if upload.format_options.webp.is_some() || upload.format_options.avif.is_some() {
                    let fo = lua.create_table()?;
                    if let Some(ref webp) = upload.format_options.webp {
                        let w = lua.create_table()?;
                        w.set("quality", webp.quality)?;
                        fo.set("webp", w)?;
                    }
                    if let Some(ref avif) = upload.format_options.avif {
                        let a = lua.create_table()?;
                        a.set("quality", avif.quality)?;
                        fo.set("avif", a)?;
                    }
                    u.set("format_options", fo)?;
                }
                tbl.set("upload", u)?;
            }
        }
    }

    // live
    match &def.live {
        None => { tbl.set("live", true)?; }
        Some(crate::core::collection::LiveSetting::Disabled) => { tbl.set("live", false)?; }
        Some(crate::core::collection::LiveSetting::Function(s)) => { tbl.set("live", s.as_str())?; }
    }

    // versions
    if let Some(ref v) = def.versions {
        if v.drafts && v.max_versions == 0 {
            tbl.set("versions", true)?;
        } else {
            let vt = lua.create_table()?;
            vt.set("drafts", v.drafts)?;
            if v.max_versions > 0 {
                vt.set("max_versions", v.max_versions)?;
            }
            tbl.set("versions", vt)?;
        }
    }

    Ok(tbl)
}

/// Convert a GlobalDefinition to a full Lua table compatible with parse_global_definition().
fn global_config_to_lua(lua: &Lua, def: &crate::core::collection::GlobalDefinition) -> mlua::Result<Table> {
    let tbl = lua.create_table()?;

    // labels
    let labels = lua.create_table()?;
    if let Some(ref s) = def.labels.singular {
        labels.set("singular", localized_string_to_lua(lua, s)?)?;
    }
    if let Some(ref s) = def.labels.plural {
        labels.set("plural", localized_string_to_lua(lua, s)?)?;
    }
    tbl.set("labels", labels)?;

    // fields
    let fields_arr = lua.create_table()?;
    for (i, f) in def.fields.iter().enumerate() {
        fields_arr.set(i + 1, field_config_to_lua(lua, f)?)?;
    }
    tbl.set("fields", fields_arr)?;

    // hooks
    tbl.set("hooks", collection_hooks_to_lua(lua, &def.hooks)?)?;

    // access
    let access = lua.create_table()?;
    if let Some(ref s) = def.access.read { access.set("read", s.as_str())?; }
    if let Some(ref s) = def.access.create { access.set("create", s.as_str())?; }
    if let Some(ref s) = def.access.update { access.set("update", s.as_str())?; }
    if let Some(ref s) = def.access.delete { access.set("delete", s.as_str())?; }
    tbl.set("access", access)?;

    // live
    match &def.live {
        None => { tbl.set("live", true)?; }
        Some(crate::core::collection::LiveSetting::Disabled) => { tbl.set("live", false)?; }
        Some(crate::core::collection::LiveSetting::Function(s)) => { tbl.set("live", s.as_str())?; }
    }

    Ok(tbl)
}

/// Convert collection-level hooks to a Lua table.
fn collection_hooks_to_lua(lua: &Lua, hooks: &crate::core::collection::CollectionHooks) -> mlua::Result<Table> {
    let tbl = lua.create_table()?;
    let pairs: &[(&str, &[String])] = &[
        ("before_validate", &hooks.before_validate),
        ("before_change", &hooks.before_change),
        ("after_change", &hooks.after_change),
        ("before_read", &hooks.before_read),
        ("after_read", &hooks.after_read),
        ("before_delete", &hooks.before_delete),
        ("after_delete", &hooks.after_delete),
        ("before_broadcast", &hooks.before_broadcast),
    ];
    for (key, list) in pairs {
        if !list.is_empty() {
            let arr = lua.create_table()?;
            for (i, s) in list.iter().enumerate() {
                arr.set(i + 1, s.as_str())?;
            }
            tbl.set(*key, arr)?;
        }
    }
    Ok(tbl)
}

/// Convert a FieldDefinition to a full Lua table compatible with parse_fields().
fn field_config_to_lua(lua: &Lua, f: &crate::core::field::FieldDefinition) -> mlua::Result<Table> {
    let tbl = lua.create_table()?;
    tbl.set("name", f.name.as_str())?;
    tbl.set("type", f.field_type.as_str())?;

    if f.required { tbl.set("required", true)?; }
    if f.unique { tbl.set("unique", true)?; }
    if f.localized { tbl.set("localized", true)?; }
    if let Some(ref v) = f.validate { tbl.set("validate", v.as_str())?; }

    if let Some(ref dv) = f.default_value {
        match dv {
            serde_json::Value::Bool(b) => { tbl.set("default_value", *b)?; }
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    tbl.set("default_value", i)?;
                } else if let Some(f_val) = n.as_f64() {
                    tbl.set("default_value", f_val)?;
                }
            }
            serde_json::Value::String(s) => { tbl.set("default_value", s.as_str())?; }
            _ => {}
        }
    }

    if let Some(ref pa) = f.picker_appearance {
        tbl.set("picker_appearance", pa.as_str())?;
    }

    // options (select fields)
    if !f.options.is_empty() {
        let opts = lua.create_table()?;
        for (i, opt) in f.options.iter().enumerate() {
            let o = lua.create_table()?;
            o.set("label", localized_string_to_lua(lua, &opt.label)?)?;
            o.set("value", opt.value.as_str())?;
            opts.set(i + 1, o)?;
        }
        tbl.set("options", opts)?;
    }

    // admin
    {
        let admin = lua.create_table()?;
        let mut has_any = false;
        if let Some(ref l) = f.admin.label {
            admin.set("label", localized_string_to_lua(lua, l)?)?;
            has_any = true;
        }
        if let Some(ref p) = f.admin.placeholder {
            admin.set("placeholder", localized_string_to_lua(lua, p)?)?;
            has_any = true;
        }
        if let Some(ref d) = f.admin.description {
            admin.set("description", localized_string_to_lua(lua, d)?)?;
            has_any = true;
        }
        if f.admin.hidden { admin.set("hidden", true)?; has_any = true; }
        if f.admin.readonly { admin.set("readonly", true)?; has_any = true; }
        if let Some(ref w) = f.admin.width {
            admin.set("width", w.as_str())?;
            has_any = true;
        }
        if f.admin.collapsed { admin.set("collapsed", true)?; has_any = true; }
        if has_any {
            tbl.set("admin", admin)?;
        }
    }

    // hooks
    {
        let hooks = lua.create_table()?;
        let mut has_any = false;
        let pairs: &[(&str, &[String])] = &[
            ("before_validate", &f.hooks.before_validate),
            ("before_change", &f.hooks.before_change),
            ("after_change", &f.hooks.after_change),
            ("after_read", &f.hooks.after_read),
        ];
        for (key, list) in pairs {
            if !list.is_empty() {
                let arr = lua.create_table()?;
                for (i, s) in list.iter().enumerate() {
                    arr.set(i + 1, s.as_str())?;
                }
                hooks.set(*key, arr)?;
                has_any = true;
            }
        }
        if has_any {
            tbl.set("hooks", hooks)?;
        }
    }

    // access
    {
        let access = lua.create_table()?;
        let mut has_any = false;
        if let Some(ref s) = f.access.read { access.set("read", s.as_str())?; has_any = true; }
        if let Some(ref s) = f.access.create { access.set("create", s.as_str())?; has_any = true; }
        if let Some(ref s) = f.access.update { access.set("update", s.as_str())?; has_any = true; }
        if has_any {
            tbl.set("access", access)?;
        }
    }

    // relationship
    if let Some(ref rc) = f.relationship {
        let rel = lua.create_table()?;
        rel.set("collection", rc.collection.as_str())?;
        if rc.has_many { rel.set("has_many", true)?; }
        if let Some(md) = rc.max_depth { rel.set("max_depth", md)?; }
        tbl.set("relationship", rel)?;
    }

    // sub-fields (array, group)
    if !f.fields.is_empty() {
        let sub = lua.create_table()?;
        for (i, sf) in f.fields.iter().enumerate() {
            sub.set(i + 1, field_config_to_lua(lua, sf)?)?;
        }
        tbl.set("fields", sub)?;
    }

    // blocks
    if !f.blocks.is_empty() {
        let blocks = lua.create_table()?;
        for (i, b) in f.blocks.iter().enumerate() {
            let bt = lua.create_table()?;
            bt.set("type", b.block_type.as_str())?;
            if let Some(ref lbl) = b.label {
                bt.set("label", localized_string_to_lua(lua, lbl)?)?;
            }
            let bf = lua.create_table()?;
            for (j, sf) in b.fields.iter().enumerate() {
                bf.set(j + 1, field_config_to_lua(lua, sf)?)?;
            }
            bt.set("fields", bf)?;
            blocks.set(i + 1, bt)?;
        }
        tbl.set("blocks", blocks)?;
    }

    Ok(tbl)
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
    use super::crypto::hex_encode;
    use serde_json::json;

    // --- hex_encode tests ---

    #[test]
    fn test_hex_encode_empty() {
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn test_hex_encode_single_byte() {
        assert_eq!(hex_encode(&[0x00]), "00");
        assert_eq!(hex_encode(&[0xff]), "ff");
        assert_eq!(hex_encode(&[0x0a]), "0a");
    }

    #[test]
    fn test_hex_encode_multiple_bytes() {
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(hex_encode(&[0x01, 0x23, 0x45, 0x67]), "01234567");
    }

    // --- lua_to_json tests ---

    #[test]
    fn test_lua_to_json_nil() {
        let lua = Lua::new();
        let result = lua_to_json(&lua, &Value::Nil).unwrap();
        assert_eq!(result, json!(null));
    }

    #[test]
    fn test_lua_to_json_boolean() {
        let lua = Lua::new();
        let result = lua_to_json(&lua, &Value::Boolean(true)).unwrap();
        assert_eq!(result, json!(true));
        let result = lua_to_json(&lua, &Value::Boolean(false)).unwrap();
        assert_eq!(result, json!(false));
    }

    #[test]
    fn test_lua_to_json_integer() {
        let lua = Lua::new();
        let result = lua_to_json(&lua, &Value::Integer(42)).unwrap();
        assert_eq!(result, json!(42));
        let result = lua_to_json(&lua, &Value::Integer(-1)).unwrap();
        assert_eq!(result, json!(-1));
    }

    #[test]
    fn test_lua_to_json_number() {
        let lua = Lua::new();
        let result = lua_to_json(&lua, &Value::Number(3.14)).unwrap();
        assert_eq!(result, json!(3.14));
    }

    #[test]
    fn test_lua_to_json_string() {
        let lua = Lua::new();
        let s = lua.create_string("hello world").unwrap();
        let result = lua_to_json(&lua, &Value::String(s)).unwrap();
        assert_eq!(result, json!("hello world"));
    }

    #[test]
    fn test_lua_to_json_array_table() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set(1, "a").unwrap();
        tbl.set(2, "b").unwrap();
        tbl.set(3, "c").unwrap();
        let result = lua_to_json(&lua, &Value::Table(tbl)).unwrap();
        assert_eq!(result, json!(["a", "b", "c"]));
    }

    #[test]
    fn test_lua_to_json_object_table() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("name", "test").unwrap();
        tbl.set("count", 42).unwrap();
        let result = lua_to_json(&lua, &Value::Table(tbl)).unwrap();
        assert_eq!(result["name"], json!("test"));
        assert_eq!(result["count"], json!(42));
    }

    #[test]
    fn test_lua_to_json_function_becomes_null() {
        let lua = Lua::new();
        let f = lua.create_function(|_, ()| Ok(())).unwrap();
        let result = lua_to_json(&lua, &Value::Function(f)).unwrap();
        assert_eq!(result, json!(null));
    }

    // --- json_to_lua tests ---

    #[test]
    fn test_json_to_lua_null() {
        let lua = Lua::new();
        let result = json_to_lua(&lua, &json!(null)).unwrap();
        assert!(matches!(result, Value::Nil));
    }

    #[test]
    fn test_json_to_lua_bool() {
        let lua = Lua::new();
        let result = json_to_lua(&lua, &json!(true)).unwrap();
        assert!(matches!(result, Value::Boolean(true)));
    }

    #[test]
    fn test_json_to_lua_integer() {
        let lua = Lua::new();
        let result = json_to_lua(&lua, &json!(42)).unwrap();
        assert!(matches!(result, Value::Integer(42)));
    }

    #[test]
    fn test_json_to_lua_float() {
        let lua = Lua::new();
        let result = json_to_lua(&lua, &json!(3.14)).unwrap();
        match result {
            Value::Number(n) => assert!((n - 3.14).abs() < f64::EPSILON),
            _ => panic!("Expected Number"),
        }
    }

    #[test]
    fn test_json_to_lua_string() {
        let lua = Lua::new();
        let result = json_to_lua(&lua, &json!("hello")).unwrap();
        match result {
            Value::String(s) => assert_eq!(s.to_str().unwrap(), "hello"),
            _ => panic!("Expected String"),
        }
    }

    #[test]
    fn test_json_to_lua_array() {
        let lua = Lua::new();
        let result = json_to_lua(&lua, &json!([1, 2, 3])).unwrap();
        match result {
            Value::Table(tbl) => {
                assert_eq!(tbl.raw_len(), 3);
                let v1: i64 = tbl.get(1).unwrap();
                let v2: i64 = tbl.get(2).unwrap();
                let v3: i64 = tbl.get(3).unwrap();
                assert_eq!(v1, 1);
                assert_eq!(v2, 2);
                assert_eq!(v3, 3);
            }
            _ => panic!("Expected Table"),
        }
    }

    #[test]
    fn test_json_to_lua_object() {
        let lua = Lua::new();
        let result = json_to_lua(&lua, &json!({"name": "test", "active": true})).unwrap();
        match result {
            Value::Table(tbl) => {
                let name: String = tbl.get("name").unwrap();
                let active: bool = tbl.get("active").unwrap();
                assert_eq!(name, "test");
                assert!(active);
            }
            _ => panic!("Expected Table"),
        }
    }

    #[test]
    fn test_json_lua_roundtrip() {
        let lua = Lua::new();
        let original = json!({
            "title": "Hello",
            "count": 42,
            "tags": ["a", "b"],
            "active": true,
            "empty": null
        });
        let lua_val = json_to_lua(&lua, &original).unwrap();
        let back = lua_to_json(&lua, &lua_val).unwrap();
        assert_eq!(back["title"], json!("Hello"));
        assert_eq!(back["count"], json!(42));
        assert_eq!(back["tags"], json!(["a", "b"]));
        assert_eq!(back["active"], json!(true));
        assert_eq!(back["empty"], json!(null));
    }

    // --- localized_string_to_lua tests ---

    #[test]
    fn test_localized_string_plain() {
        let lua = Lua::new();
        let ls = crate::core::field::LocalizedString::Plain("Hello".to_string());
        let result = localized_string_to_lua(&lua, &ls).unwrap();
        match result {
            Value::String(s) => assert_eq!(s.to_str().unwrap(), "Hello"),
            _ => panic!("Expected String"),
        }
    }

    #[test]
    fn test_localized_string_localized() {
        let lua = Lua::new();
        let mut map = std::collections::HashMap::new();
        map.insert("en".to_string(), "Hello".to_string());
        map.insert("de".to_string(), "Hallo".to_string());
        let ls = crate::core::field::LocalizedString::Localized(map);
        let result = localized_string_to_lua(&lua, &ls).unwrap();
        match result {
            Value::Table(tbl) => {
                let en: String = tbl.get("en").unwrap();
                let de: String = tbl.get("de").unwrap();
                assert_eq!(en, "Hello");
                assert_eq!(de, "Hallo");
            }
            _ => panic!("Expected Table"),
        }
    }

    // --- collection_config_to_lua round-trip tests ---

    #[test]
    fn test_collection_config_to_lua_basic() {
        let lua = Lua::new();
        let def = crate::core::CollectionDefinition {
            slug: "posts".to_string(),
            labels: crate::core::collection::CollectionLabels {
                singular: Some(crate::core::field::LocalizedString::Plain("Post".to_string())),
                plural: Some(crate::core::field::LocalizedString::Plain("Posts".to_string())),
            },
            timestamps: true,
            fields: vec![
                crate::core::field::FieldDefinition {
                    name: "title".to_string(),
                    field_type: crate::core::field::FieldType::Text,
                    required: true,
                    ..Default::default()
                },
            ],
            admin: crate::core::collection::CollectionAdmin::default(),
            hooks: crate::core::collection::CollectionHooks::default(),
            auth: None,
            upload: None,
            access: crate::core::collection::CollectionAccess::default(),
            live: None,
            versions: None,
        };
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let labels: mlua::Table = tbl.get("labels").unwrap();
        let singular: String = labels.get("singular").unwrap();
        assert_eq!(singular, "Post");
        assert_eq!(tbl.get::<bool>("timestamps").unwrap(), true);

        let fields: mlua::Table = tbl.get("fields").unwrap();
        let f1: mlua::Table = fields.get(1).unwrap();
        let fname: String = f1.get("name").unwrap();
        assert_eq!(fname, "title");
    }

    #[test]
    fn test_collection_config_to_lua_with_auth_simple() {
        let lua = Lua::new();
        let def = crate::core::CollectionDefinition {
            slug: "users".to_string(),
            labels: crate::core::collection::CollectionLabels::default(),
            timestamps: true,
            fields: Vec::new(),
            admin: crate::core::collection::CollectionAdmin::default(),
            hooks: crate::core::collection::CollectionHooks::default(),
            auth: Some(crate::core::collection::CollectionAuth {
                enabled: true,
                ..Default::default()
            }),
            upload: None,
            access: crate::core::collection::CollectionAccess::default(),
            live: None,
            versions: None,
        };
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        // Simple auth = true should be serialized as bool true
        let auth_val: bool = tbl.get("auth").unwrap();
        assert!(auth_val);
    }

    #[test]
    fn test_collection_config_to_lua_with_auth_complex() {
        let lua = Lua::new();
        let def = crate::core::CollectionDefinition {
            slug: "users".to_string(),
            labels: crate::core::collection::CollectionLabels::default(),
            timestamps: true,
            fields: Vec::new(),
            admin: crate::core::collection::CollectionAdmin::default(),
            hooks: crate::core::collection::CollectionHooks::default(),
            auth: Some(crate::core::collection::CollectionAuth {
                enabled: true,
                token_expiry: 3600,
                disable_local: true,
                verify_email: true,
                forgot_password: false,
                strategies: vec![crate::core::collection::AuthStrategy {
                    name: "oauth".to_string(),
                    authenticate: "hooks.auth.oauth".to_string(),
                }],
            }),
            upload: None,
            access: crate::core::collection::CollectionAccess::default(),
            live: None,
            versions: None,
        };
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let auth_tbl: mlua::Table = tbl.get("auth").unwrap();
        assert_eq!(auth_tbl.get::<u64>("token_expiry").unwrap(), 3600);
        assert_eq!(auth_tbl.get::<bool>("disable_local").unwrap(), true);
        assert_eq!(auth_tbl.get::<bool>("verify_email").unwrap(), true);
        assert_eq!(auth_tbl.get::<bool>("forgot_password").unwrap(), false);
        let strats: mlua::Table = auth_tbl.get("strategies").unwrap();
        let s1: mlua::Table = strats.get(1).unwrap();
        let sname: String = s1.get("name").unwrap();
        assert_eq!(sname, "oauth");
    }

    #[test]
    fn test_collection_config_to_lua_with_upload() {
        let lua = Lua::new();
        let def = crate::core::CollectionDefinition {
            slug: "media".to_string(),
            labels: crate::core::collection::CollectionLabels::default(),
            timestamps: true,
            fields: Vec::new(),
            admin: crate::core::collection::CollectionAdmin::default(),
            hooks: crate::core::collection::CollectionHooks::default(),
            auth: None,
            upload: Some(crate::core::upload::CollectionUpload {
                enabled: true,
                mime_types: vec!["image/png".to_string()],
                max_file_size: Some(1000000),
                image_sizes: vec![crate::core::upload::ImageSize {
                    name: "thumb".to_string(),
                    width: 200,
                    height: 200,
                    fit: crate::core::upload::ImageFit::Cover,
                }],
                admin_thumbnail: Some("thumb".to_string()),
                format_options: crate::core::upload::FormatOptions {
                    webp: Some(crate::core::upload::FormatQuality { quality: 80 }),
                    avif: None,
                },
            }),
            access: crate::core::collection::CollectionAccess::default(),
            live: None,
            versions: None,
        };
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let upload: mlua::Table = tbl.get("upload").unwrap();
        let mt: mlua::Table = upload.get("mime_types").unwrap();
        let m1: String = mt.get(1).unwrap();
        assert_eq!(m1, "image/png");
        assert_eq!(upload.get::<u64>("max_file_size").unwrap(), 1000000);
        let sizes: mlua::Table = upload.get("image_sizes").unwrap();
        let s1: mlua::Table = sizes.get(1).unwrap();
        assert_eq!(s1.get::<String>("name").unwrap(), "thumb");
        assert_eq!(s1.get::<String>("fit").unwrap(), "cover");
        let fo: mlua::Table = upload.get("format_options").unwrap();
        let webp: mlua::Table = fo.get("webp").unwrap();
        assert_eq!(webp.get::<u8>("quality").unwrap(), 80);
    }

    #[test]
    fn test_collection_config_to_lua_live_settings() {
        let lua = Lua::new();

        // live = None -> true
        let def_none_live = crate::core::CollectionDefinition {
            slug: "t".to_string(),
            live: None,
            labels: crate::core::collection::CollectionLabels::default(),
            timestamps: true,
            fields: Vec::new(),
            admin: crate::core::collection::CollectionAdmin::default(),
            hooks: crate::core::collection::CollectionHooks::default(),
            auth: None,
            upload: None,
            access: crate::core::collection::CollectionAccess::default(),
            versions: None,
        };
        let tbl = collection_config_to_lua(&lua, &def_none_live).unwrap();
        assert_eq!(tbl.get::<bool>("live").unwrap(), true);

        // live = Disabled -> false
        let def_disabled = crate::core::CollectionDefinition {
            slug: "t".to_string(),
            live: Some(crate::core::collection::LiveSetting::Disabled),
            labels: crate::core::collection::CollectionLabels::default(),
            timestamps: true,
            fields: Vec::new(),
            admin: crate::core::collection::CollectionAdmin::default(),
            hooks: crate::core::collection::CollectionHooks::default(),
            auth: None,
            upload: None,
            access: crate::core::collection::CollectionAccess::default(),
            versions: None,
        };
        let tbl = collection_config_to_lua(&lua, &def_disabled).unwrap();
        assert_eq!(tbl.get::<bool>("live").unwrap(), false);

        // live = Function -> string
        let def_func = crate::core::CollectionDefinition {
            slug: "t".to_string(),
            live: Some(crate::core::collection::LiveSetting::Function("hooks.live.filter".to_string())),
            labels: crate::core::collection::CollectionLabels::default(),
            timestamps: true,
            fields: Vec::new(),
            admin: crate::core::collection::CollectionAdmin::default(),
            hooks: crate::core::collection::CollectionHooks::default(),
            auth: None,
            upload: None,
            access: crate::core::collection::CollectionAccess::default(),
            versions: None,
        };
        let tbl = collection_config_to_lua(&lua, &def_func).unwrap();
        assert_eq!(tbl.get::<String>("live").unwrap(), "hooks.live.filter");
    }

    #[test]
    fn test_collection_config_to_lua_versions() {
        let lua = Lua::new();

        // versions simple (drafts=true, max=0) -> true
        let def = crate::core::CollectionDefinition {
            slug: "t".to_string(),
            labels: crate::core::collection::CollectionLabels::default(),
            timestamps: true,
            fields: Vec::new(),
            admin: crate::core::collection::CollectionAdmin::default(),
            hooks: crate::core::collection::CollectionHooks::default(),
            auth: None,
            upload: None,
            access: crate::core::collection::CollectionAccess::default(),
            live: None,
            versions: Some(crate::core::collection::VersionsConfig {
                drafts: true,
                max_versions: 0,
            }),
        };
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        assert_eq!(tbl.get::<bool>("versions").unwrap(), true);

        // versions table
        let def2 = crate::core::CollectionDefinition {
            slug: "t".to_string(),
            labels: crate::core::collection::CollectionLabels::default(),
            timestamps: true,
            fields: Vec::new(),
            admin: crate::core::collection::CollectionAdmin::default(),
            hooks: crate::core::collection::CollectionHooks::default(),
            auth: None,
            upload: None,
            access: crate::core::collection::CollectionAccess::default(),
            live: None,
            versions: Some(crate::core::collection::VersionsConfig {
                drafts: false,
                max_versions: 100,
            }),
        };
        let tbl = collection_config_to_lua(&lua, &def2).unwrap();
        let v: mlua::Table = tbl.get("versions").unwrap();
        assert_eq!(v.get::<bool>("drafts").unwrap(), false);
        assert_eq!(v.get::<u32>("max_versions").unwrap(), 100);
    }

    // --- global_config_to_lua tests ---

    #[test]
    fn test_global_config_to_lua_basic() {
        let lua = Lua::new();
        let def = crate::core::collection::GlobalDefinition {
            slug: "settings".to_string(),
            labels: crate::core::collection::CollectionLabels {
                singular: Some(crate::core::field::LocalizedString::Plain("Settings".to_string())),
                plural: None,
            },
            fields: vec![crate::core::field::FieldDefinition {
                name: "site_name".to_string(),
                ..Default::default()
            }],
            hooks: crate::core::collection::CollectionHooks::default(),
            access: crate::core::collection::CollectionAccess::default(),
            live: None,
            versions: None,
        };
        let tbl = global_config_to_lua(&lua, &def).unwrap();
        let labels: mlua::Table = tbl.get("labels").unwrap();
        let singular: String = labels.get("singular").unwrap();
        assert_eq!(singular, "Settings");
        let fields: mlua::Table = tbl.get("fields").unwrap();
        let f1: mlua::Table = fields.get(1).unwrap();
        assert_eq!(f1.get::<String>("name").unwrap(), "site_name");
    }

    #[test]
    fn test_global_config_to_lua_with_live() {
        let lua = Lua::new();
        let def = crate::core::collection::GlobalDefinition {
            slug: "settings".to_string(),
            labels: crate::core::collection::CollectionLabels::default(),
            fields: Vec::new(),
            hooks: crate::core::collection::CollectionHooks::default(),
            access: crate::core::collection::CollectionAccess {
                read: Some("hooks.access.allow".to_string()),
                ..Default::default()
            },
            live: Some(crate::core::collection::LiveSetting::Disabled),
            versions: None,
        };
        let tbl = global_config_to_lua(&lua, &def).unwrap();
        assert_eq!(tbl.get::<bool>("live").unwrap(), false);
        let access: mlua::Table = tbl.get("access").unwrap();
        assert_eq!(access.get::<String>("read").unwrap(), "hooks.access.allow");
    }

    // --- field_config_to_lua tests ---

    #[test]
    fn test_field_config_to_lua_simple() {
        let lua = Lua::new();
        let f = crate::core::field::FieldDefinition {
            name: "title".to_string(),
            field_type: crate::core::field::FieldType::Text,
            required: true,
            unique: true,
            validate: Some("hooks.validate.title_check".to_string()),
            default_value: Some(serde_json::json!("untitled")),
            ..Default::default()
        };
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        assert_eq!(tbl.get::<String>("name").unwrap(), "title");
        assert_eq!(tbl.get::<String>("type").unwrap(), "text");
        assert_eq!(tbl.get::<bool>("required").unwrap(), true);
        assert_eq!(tbl.get::<bool>("unique").unwrap(), true);
        assert_eq!(tbl.get::<String>("validate").unwrap(), "hooks.validate.title_check");
        assert_eq!(tbl.get::<String>("default_value").unwrap(), "untitled");
    }

    #[test]
    fn test_field_config_to_lua_with_relationship() {
        let lua = Lua::new();
        let f = crate::core::field::FieldDefinition {
            name: "author".to_string(),
            field_type: crate::core::field::FieldType::Relationship,
            relationship: Some(crate::core::field::RelationshipConfig {
                collection: "users".to_string(),
                has_many: true,
                max_depth: Some(2),
            }),
            ..Default::default()
        };
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        let rel: mlua::Table = tbl.get("relationship").unwrap();
        assert_eq!(rel.get::<String>("collection").unwrap(), "users");
        assert_eq!(rel.get::<bool>("has_many").unwrap(), true);
        assert_eq!(rel.get::<i32>("max_depth").unwrap(), 2);
    }

    #[test]
    fn test_field_config_to_lua_with_options() {
        let lua = Lua::new();
        let f = crate::core::field::FieldDefinition {
            name: "status".to_string(),
            field_type: crate::core::field::FieldType::Select,
            options: vec![
                crate::core::field::SelectOption {
                    label: crate::core::field::LocalizedString::Plain("Draft".to_string()),
                    value: "draft".to_string(),
                },
                crate::core::field::SelectOption {
                    label: crate::core::field::LocalizedString::Plain("Published".to_string()),
                    value: "published".to_string(),
                },
            ],
            ..Default::default()
        };
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        let opts: mlua::Table = tbl.get("options").unwrap();
        let o1: mlua::Table = opts.get(1).unwrap();
        assert_eq!(o1.get::<String>("value").unwrap(), "draft");
    }

    #[test]
    fn test_field_config_to_lua_with_blocks() {
        let lua = Lua::new();
        let f = crate::core::field::FieldDefinition {
            name: "content".to_string(),
            field_type: crate::core::field::FieldType::Blocks,
            blocks: vec![crate::core::field::BlockDefinition {
                block_type: "text".to_string(),
                label: Some(crate::core::field::LocalizedString::Plain("Text Block".to_string())),
                fields: vec![crate::core::field::FieldDefinition {
                    name: "body".to_string(),
                    ..Default::default()
                }],
                label_field: None,
            }],
            ..Default::default()
        };
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        let blocks: mlua::Table = tbl.get("blocks").unwrap();
        let b1: mlua::Table = blocks.get(1).unwrap();
        assert_eq!(b1.get::<String>("type").unwrap(), "text");
        assert_eq!(b1.get::<String>("label").unwrap(), "Text Block");
        let bf: mlua::Table = b1.get("fields").unwrap();
        let bf1: mlua::Table = bf.get(1).unwrap();
        assert_eq!(bf1.get::<String>("name").unwrap(), "body");
    }

    #[test]
    fn test_field_config_to_lua_with_admin_and_hooks() {
        let lua = Lua::new();
        let f = crate::core::field::FieldDefinition {
            name: "title".to_string(),
            admin: crate::core::field::FieldAdmin {
                label: Some(crate::core::field::LocalizedString::Plain("Title".to_string())),
                placeholder: Some(crate::core::field::LocalizedString::Plain("Enter title".to_string())),
                description: Some(crate::core::field::LocalizedString::Plain("The document title".to_string())),
                hidden: true,
                readonly: true,
                width: Some("50%".to_string()),
                collapsed: true,
                ..Default::default()
            },
            hooks: crate::core::field::FieldHooks {
                before_validate: vec!["hooks.field.trim".to_string()],
                before_change: vec!["hooks.field.upper".to_string()],
                after_change: Vec::new(),
                after_read: vec!["hooks.field.format".to_string()],
            },
            access: crate::core::field::FieldAccess {
                read: Some("hooks.access.check".to_string()),
                create: Some("hooks.access.admin".to_string()),
                update: None,
            },
            ..Default::default()
        };
        let tbl = field_config_to_lua(&lua, &f).unwrap();

        let admin: mlua::Table = tbl.get("admin").unwrap();
        assert_eq!(admin.get::<String>("label").unwrap(), "Title");
        assert_eq!(admin.get::<bool>("hidden").unwrap(), true);
        assert_eq!(admin.get::<bool>("readonly").unwrap(), true);
        assert_eq!(admin.get::<String>("width").unwrap(), "50%");
        assert_eq!(admin.get::<bool>("collapsed").unwrap(), true);

        let hooks: mlua::Table = tbl.get("hooks").unwrap();
        let bv: mlua::Table = hooks.get("before_validate").unwrap();
        assert_eq!(bv.get::<String>(1).unwrap(), "hooks.field.trim");
        let bc: mlua::Table = hooks.get("before_change").unwrap();
        assert_eq!(bc.get::<String>(1).unwrap(), "hooks.field.upper");
        let ar: mlua::Table = hooks.get("after_read").unwrap();
        assert_eq!(ar.get::<String>(1).unwrap(), "hooks.field.format");

        let access: mlua::Table = tbl.get("access").unwrap();
        assert_eq!(access.get::<String>("read").unwrap(), "hooks.access.check");
        assert_eq!(access.get::<String>("create").unwrap(), "hooks.access.admin");
    }

    #[test]
    fn test_field_config_to_lua_default_values() {
        let lua = Lua::new();

        // Bool default
        let f_bool = crate::core::field::FieldDefinition {
            name: "active".to_string(),
            default_value: Some(serde_json::json!(true)),
            ..Default::default()
        };
        let tbl = field_config_to_lua(&lua, &f_bool).unwrap();
        assert_eq!(tbl.get::<bool>("default_value").unwrap(), true);

        // Integer default
        let f_int = crate::core::field::FieldDefinition {
            name: "count".to_string(),
            default_value: Some(serde_json::json!(42)),
            ..Default::default()
        };
        let tbl = field_config_to_lua(&lua, &f_int).unwrap();
        assert_eq!(tbl.get::<i64>("default_value").unwrap(), 42);

        // Float default
        let f_float = crate::core::field::FieldDefinition {
            name: "price".to_string(),
            default_value: Some(serde_json::json!(3.14)),
            ..Default::default()
        };
        let tbl = field_config_to_lua(&lua, &f_float).unwrap();
        let val: f64 = tbl.get("default_value").unwrap();
        assert!((val - 3.14).abs() < f64::EPSILON);
    }

    // --- collection_hooks_to_lua tests ---

    #[test]
    fn test_collection_hooks_to_lua() {
        let lua = Lua::new();
        let hooks = crate::core::collection::CollectionHooks {
            before_validate: vec!["hooks.v".to_string()],
            before_change: vec!["hooks.c1".to_string(), "hooks.c2".to_string()],
            after_change: Vec::new(),
            before_read: Vec::new(),
            after_read: Vec::new(),
            before_delete: Vec::new(),
            after_delete: Vec::new(),
            before_broadcast: vec!["hooks.b".to_string()],
        };
        let tbl = collection_hooks_to_lua(&lua, &hooks).unwrap();
        let bv: mlua::Table = tbl.get("before_validate").unwrap();
        assert_eq!(bv.raw_len(), 1);
        let bc: mlua::Table = tbl.get("before_change").unwrap();
        assert_eq!(bc.raw_len(), 2);
        let bb: mlua::Table = tbl.get("before_broadcast").unwrap();
        assert_eq!(bb.raw_len(), 1);
        // Empty hooks should not have entries
        let ac: Value = tbl.get("after_change").unwrap();
        assert!(matches!(ac, Value::Nil));
    }

    #[test]
    fn test_collection_config_to_lua_with_admin() {
        let lua = Lua::new();
        let def = crate::core::CollectionDefinition {
            slug: "posts".to_string(),
            labels: crate::core::collection::CollectionLabels::default(),
            timestamps: true,
            fields: Vec::new(),
            admin: crate::core::collection::CollectionAdmin {
                use_as_title: Some("title".to_string()),
                default_sort: Some("-created_at".to_string()),
                hidden: true,
                list_searchable_fields: vec!["title".to_string(), "body".to_string()],
            },
            hooks: crate::core::collection::CollectionHooks::default(),
            auth: None,
            upload: None,
            access: crate::core::collection::CollectionAccess::default(),
            live: None,
            versions: None,
        };
        let tbl = collection_config_to_lua(&lua, &def).unwrap();
        let admin: mlua::Table = tbl.get("admin").unwrap();
        assert_eq!(admin.get::<String>("use_as_title").unwrap(), "title");
        assert_eq!(admin.get::<String>("default_sort").unwrap(), "-created_at");
        assert_eq!(admin.get::<bool>("hidden").unwrap(), true);
        let lsf: mlua::Table = admin.get("list_searchable_fields").unwrap();
        assert_eq!(lsf.raw_len(), 2);
    }
}
