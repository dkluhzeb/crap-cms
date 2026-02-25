//! Registers the `crap.*` Lua API namespace (collections, globals, hooks, log, util,
//! crypto, schema).

pub mod parse;

use anyhow::{Context, Result};
use mlua::{Lua, Table, Value, Function};
use std::path::Path;

use crate::config::CrapConfig;
use crate::core::SharedRegistry;

use parse::{parse_collection_definition, parse_global_definition};

/// Pure Lua table and string helpers, loaded onto `crap.util` after the table is set.
const LUA_UTIL_HELPERS: &str = r#"
local util = crap.util

function util.deep_merge(a, b)
    local out = {}
    for k, v in pairs(a) do
        out[k] = v
    end
    for k, v in pairs(b) do
        if type(out[k]) == "table" and type(v) == "table" then
            out[k] = util.deep_merge(out[k], v)
        else
            out[k] = v
        end
    end
    return out
end

function util.pick(tbl, keys)
    local out = {}
    for _, k in ipairs(keys) do
        out[k] = tbl[k]
    end
    return out
end

function util.omit(tbl, keys)
    local skip = {}
    for _, k in ipairs(keys) do skip[k] = true end
    local out = {}
    for k, v in pairs(tbl) do
        if not skip[k] then out[k] = v end
    end
    return out
end

function util.keys(tbl)
    local out = {}
    for k in pairs(tbl) do out[#out + 1] = k end
    return out
end

function util.values(tbl)
    local out = {}
    for _, v in pairs(tbl) do out[#out + 1] = v end
    return out
end

function util.map(tbl, fn)
    local out = {}
    for i, v in ipairs(tbl) do out[i] = fn(v, i) end
    return out
end

function util.filter(tbl, fn)
    local out = {}
    for i, v in ipairs(tbl) do
        if fn(v, i) then out[#out + 1] = v end
    end
    return out
end

function util.find(tbl, fn)
    for i, v in ipairs(tbl) do
        if fn(v, i) then return v end
    end
    return nil
end

function util.includes(tbl, value)
    for _, v in ipairs(tbl) do
        if v == value then return true end
    end
    return false
end

function util.is_empty(tbl)
    return next(tbl) == nil
end

function util.clone(tbl)
    local out = {}
    for k, v in pairs(tbl) do out[k] = v end
    return out
end

function util.trim(str)
    return (str:gsub("^%s+", ""):gsub("%s+$", ""))
end

function util.split(str, sep)
    local out = {}
    local pattern = "([^" .. sep .. "]+)"
    for part in str:gmatch(pattern) do
        out[#out + 1] = part
    end
    return out
end

function util.starts_with(str, prefix)
    return str:sub(1, #prefix) == prefix
end

function util.ends_with(str, suffix)
    return suffix == "" or str:sub(-#suffix) == suffix
end

function util.truncate(str, max_len, suffix)
    suffix = suffix or "..."
    if #str <= max_len then return str end
    return str:sub(1, max_len - #suffix) .. suffix
end
"#;

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

    // Date helpers (Rust, using chrono)
    {
        let date_now_fn = lua.create_function(|_, ()| -> mlua::Result<String> {
            Ok(chrono::Utc::now().to_rfc3339())
        })?;
        util_table.set("date_now", date_now_fn)?;

        let date_timestamp_fn = lua.create_function(|_, ()| -> mlua::Result<i64> {
            Ok(chrono::Utc::now().timestamp())
        })?;
        util_table.set("date_timestamp", date_timestamp_fn)?;

        let date_parse_fn = lua.create_function(|_, s: String| -> mlua::Result<i64> {
            // Try RFC 3339 first
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&s) {
                return Ok(dt.timestamp());
            }
            // Try "YYYY-MM-DD HH:MM:SS"
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S") {
                return Ok(dt.and_utc().timestamp());
            }
            // Try "YYYY-MM-DD"
            if let Ok(d) = chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
                return Ok(d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp());
            }
            Err(mlua::Error::RuntimeError(format!("could not parse date: {}", s)))
        })?;
        util_table.set("date_parse", date_parse_fn)?;

        let date_format_fn = lua.create_function(|_, (ts, fmt): (i64, String)| -> mlua::Result<String> {
            let dt = chrono::DateTime::from_timestamp(ts, 0)
                .ok_or_else(|| mlua::Error::RuntimeError("invalid timestamp".into()))?;
            Ok(dt.format(&fmt).to_string())
        })?;
        util_table.set("date_format", date_format_fn)?;

        let date_add_fn = lua.create_function(|_, (ts, secs): (i64, i64)| -> mlua::Result<i64> {
            Ok(ts + secs)
        })?;
        util_table.set("date_add", date_add_fn)?;

        let date_diff_fn = lua.create_function(|_, (a, b): (i64, i64)| -> mlua::Result<i64> {
            Ok(a - b)
        })?;
        util_table.set("date_diff", date_diff_fn)?;
    }

    crap.set("util", util_table)?;

    // crap.crypto — hashing, encoding, encryption
    {
        let crypto_table = lua.create_table()?;

        let sha256_fn = lua.create_function(|_, data: String| -> mlua::Result<String> {
            use ring::digest;
            let hash = digest::digest(&digest::SHA256, data.as_bytes());
            Ok(hex_encode(hash.as_ref()))
        })?;
        crypto_table.set("sha256", sha256_fn)?;

        let hmac_sha256_fn = lua.create_function(|_, (data, key): (String, String)| -> mlua::Result<String> {
            use ring::hmac;
            let k = hmac::Key::new(hmac::HMAC_SHA256, key.as_bytes());
            let tag = hmac::sign(&k, data.as_bytes());
            Ok(hex_encode(tag.as_ref()))
        })?;
        crypto_table.set("hmac_sha256", hmac_sha256_fn)?;

        let b64_encode_fn = lua.create_function(|_, data: String| -> mlua::Result<String> {
            use base64::Engine;
            Ok(base64::engine::general_purpose::STANDARD.encode(data.as_bytes()))
        })?;
        crypto_table.set("base64_encode", b64_encode_fn)?;

        let b64_decode_fn = lua.create_function(|_, data: String| -> mlua::Result<String> {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD.decode(data.as_bytes())
                .map_err(|e| mlua::Error::RuntimeError(format!("base64 decode error: {}", e)))?;
            String::from_utf8(bytes)
                .map_err(|e| mlua::Error::RuntimeError(format!("base64 decode utf8 error: {}", e)))
        })?;
        crypto_table.set("base64_decode", b64_decode_fn)?;

        let auth_secret = config.auth.secret.clone();
        let encrypt_fn = lua.create_function(move |_, plaintext: String| -> mlua::Result<String> {
            use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
            use aes_gcm::Nonce;
            use ring::digest;
            use base64::Engine;
            use rand::RngCore;

            // Derive a 32-byte key from auth.secret via SHA-256
            let key_hash = digest::digest(&digest::SHA256, auth_secret.as_bytes());
            let cipher = Aes256Gcm::new_from_slice(key_hash.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("cipher init: {}", e)))?;

            // Random 12-byte nonce
            let mut nonce_bytes = [0u8; 12];
            rand::thread_rng().fill_bytes(&mut nonce_bytes);
            let nonce = Nonce::from_slice(&nonce_bytes);

            let ciphertext = cipher.encrypt(nonce, plaintext.as_bytes())
                .map_err(|e| mlua::Error::RuntimeError(format!("encrypt error: {}", e)))?;

            // Prepend nonce to ciphertext, base64 encode
            let mut combined = nonce_bytes.to_vec();
            combined.extend_from_slice(&ciphertext);
            Ok(base64::engine::general_purpose::STANDARD.encode(&combined))
        })?;
        crypto_table.set("encrypt", encrypt_fn)?;

        let auth_secret2 = config.auth.secret.clone();
        let decrypt_fn = lua.create_function(move |_, encoded: String| -> mlua::Result<String> {
            use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
            use aes_gcm::Nonce;
            use ring::digest;
            use base64::Engine;

            let combined = base64::engine::general_purpose::STANDARD.decode(encoded.as_bytes())
                .map_err(|e| mlua::Error::RuntimeError(format!("base64 decode: {}", e)))?;
            if combined.len() < 12 {
                return Err(mlua::Error::RuntimeError("ciphertext too short".into()));
            }
            let (nonce_bytes, ciphertext) = combined.split_at(12);
            let nonce = Nonce::from_slice(nonce_bytes);

            let key_hash = digest::digest(&digest::SHA256, auth_secret2.as_bytes());
            let cipher = Aes256Gcm::new_from_slice(key_hash.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("cipher init: {}", e)))?;

            let plaintext = cipher.decrypt(nonce, ciphertext)
                .map_err(|e| mlua::Error::RuntimeError(format!("decrypt error: {}", e)))?;

            String::from_utf8(plaintext)
                .map_err(|e| mlua::Error::RuntimeError(format!("decrypt utf8: {}", e)))
        })?;
        crypto_table.set("decrypt", decrypt_fn)?;

        let random_bytes_fn = lua.create_function(|_, n: usize| -> mlua::Result<String> {
            use rand::RngCore;
            let mut buf = vec![0u8; n];
            rand::thread_rng().fill_bytes(&mut buf);
            Ok(hex_encode(&buf))
        })?;
        crypto_table.set("random_bytes", random_bytes_fn)?;

        crap.set("crypto", crypto_table)?;
    }

    // crap.schema — read-only schema introspection
    {
        let schema_table = lua.create_table()?;

        let reg = registry.clone();
        let get_collection_fn = lua.create_function(move |lua, slug: String| -> mlua::Result<Value> {
            let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                format!("Registry lock: {}", e)
            ))?;
            match r.get_collection(&slug) {
                Some(def) => Ok(Value::Table(collection_def_to_lua_table(lua, def)?)),
                None => Ok(Value::Nil),
            }
        })?;
        schema_table.set("get_collection", get_collection_fn)?;

        let reg = registry.clone();
        let get_global_fn = lua.create_function(move |lua, slug: String| -> mlua::Result<Value> {
            let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                format!("Registry lock: {}", e)
            ))?;
            match r.get_global(&slug) {
                Some(def) => {
                    let tbl = lua.create_table()?;
                    tbl.set("slug", def.slug.as_str())?;
                    let labels = lua.create_table()?;
                    if let Some(ref s) = def.labels.singular {
                        labels.set("singular", s.resolve_default())?;
                    }
                    if let Some(ref s) = def.labels.plural {
                        labels.set("plural", s.resolve_default())?;
                    }
                    tbl.set("labels", labels)?;
                    let fields_arr = lua.create_table()?;
                    for (i, f) in def.fields.iter().enumerate() {
                        fields_arr.set(i + 1, field_def_to_lua_table(lua, f)?)?;
                    }
                    tbl.set("fields", fields_arr)?;
                    Ok(Value::Table(tbl))
                }
                None => Ok(Value::Nil),
            }
        })?;
        schema_table.set("get_global", get_global_fn)?;

        let reg = registry.clone();
        let list_collections_fn = lua.create_function(move |lua, ()| -> mlua::Result<Table> {
            let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                format!("Registry lock: {}", e)
            ))?;
            let tbl = lua.create_table()?;
            let mut i = 0;
            for def in r.collections.values() {
                i += 1;
                let item = lua.create_table()?;
                item.set("slug", def.slug.as_str())?;
                let labels = lua.create_table()?;
                if let Some(ref s) = def.labels.singular {
                    labels.set("singular", s.resolve_default())?;
                }
                if let Some(ref s) = def.labels.plural {
                    labels.set("plural", s.resolve_default())?;
                }
                item.set("labels", labels)?;
                tbl.set(i, item)?;
            }
            Ok(tbl)
        })?;
        schema_table.set("list_collections", list_collections_fn)?;

        let reg = registry.clone();
        let list_globals_fn = lua.create_function(move |lua, ()| -> mlua::Result<Table> {
            let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
                format!("Registry lock: {}", e)
            ))?;
            let tbl = lua.create_table()?;
            let mut i = 0;
            for def in r.globals.values() {
                i += 1;
                let item = lua.create_table()?;
                item.set("slug", def.slug.as_str())?;
                let labels = lua.create_table()?;
                if let Some(ref s) = def.labels.singular {
                    labels.set("singular", s.resolve_default())?;
                }
                if let Some(ref s) = def.labels.plural {
                    labels.set("plural", s.resolve_default())?;
                }
                item.set("labels", labels)?;
                tbl.set(i, item)?;
            }
            Ok(tbl)
        })?;
        schema_table.set("list_globals", list_globals_fn)?;

        crap.set("schema", schema_table)?;
    }

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

    // Load pure Lua helpers onto crap.util (after crap global is set)
    lua.load(LUA_UTIL_HELPERS).exec()
        .context("Failed to load Lua util helpers")?;

    Ok(())
}

/// Encode bytes as lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Convert a CollectionDefinition to a Lua table for crap.schema.get_collection().
fn collection_def_to_lua_table(lua: &Lua, def: &crate::core::CollectionDefinition) -> mlua::Result<Table> {
    let tbl = lua.create_table()?;
    tbl.set("slug", def.slug.as_str())?;
    let labels = lua.create_table()?;
    if let Some(ref s) = def.labels.singular {
        labels.set("singular", s.resolve_default())?;
    }
    if let Some(ref s) = def.labels.plural {
        labels.set("plural", s.resolve_default())?;
    }
    tbl.set("labels", labels)?;
    tbl.set("timestamps", def.timestamps)?;
    tbl.set("has_auth", def.is_auth_collection())?;
    tbl.set("has_upload", def.is_upload_collection())?;
    tbl.set("has_versions", def.has_versions())?;
    tbl.set("has_drafts", def.has_drafts())?;

    let fields_arr = lua.create_table()?;
    for (i, f) in def.fields.iter().enumerate() {
        fields_arr.set(i + 1, field_def_to_lua_table(lua, f)?)?;
    }
    tbl.set("fields", fields_arr)?;
    Ok(tbl)
}

/// Convert a FieldDefinition to a Lua table for schema introspection.
fn field_def_to_lua_table(lua: &Lua, f: &crate::core::field::FieldDefinition) -> mlua::Result<Table> {
    let tbl = lua.create_table()?;
    tbl.set("name", f.name.as_str())?;
    tbl.set("type", f.field_type.as_str())?;
    tbl.set("required", f.required)?;
    tbl.set("localized", f.localized)?;
    tbl.set("unique", f.unique)?;

    if let Some(ref rc) = f.relationship {
        let rel = lua.create_table()?;
        rel.set("collection", rc.collection.as_str())?;
        rel.set("has_many", rc.has_many)?;
        if let Some(md) = rc.max_depth {
            rel.set("max_depth", md)?;
        }
        tbl.set("relationship", rel)?;
    }

    if !f.options.is_empty() {
        let opts = lua.create_table()?;
        for (i, opt) in f.options.iter().enumerate() {
            let o = lua.create_table()?;
            o.set("label", opt.label.resolve_default())?;
            o.set("value", opt.value.as_str())?;
            opts.set(i + 1, o)?;
        }
        tbl.set("options", opts)?;
    }

    // Recurse into sub-fields (array, group)
    if !f.fields.is_empty() {
        let sub = lua.create_table()?;
        for (i, sf) in f.fields.iter().enumerate() {
            sub.set(i + 1, field_def_to_lua_table(lua, sf)?)?;
        }
        tbl.set("fields", sub)?;
    }

    // Blocks
    if !f.blocks.is_empty() {
        let blocks = lua.create_table()?;
        for (i, b) in f.blocks.iter().enumerate() {
            let bt = lua.create_table()?;
            bt.set("type", b.block_type.as_str())?;
            if let Some(ref lbl) = b.label {
                bt.set("label", lbl.resolve_default())?;
            }
            let bf = lua.create_table()?;
            for (j, sf) in b.fields.iter().enumerate() {
                bf.set(j + 1, field_def_to_lua_table(lua, sf)?)?;
            }
            bt.set("fields", bf)?;
            blocks.set(i + 1, bt)?;
        }
        tbl.set("blocks", blocks)?;
    }

    Ok(tbl)
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
