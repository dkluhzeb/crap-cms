//! Registration of `crap.globals.get`, `crap.globals.update`, and `crap.jobs.queue` Lua functions.

use anyhow::Result;
use mlua::{Lua, Value};

use crate::config::LocaleConfig;
use crate::core::SharedRegistry;
use crate::db::query::{self, LocaleContext};

use super::get_tx_conn;
use crate::hooks::lifecycle::converters::*;

/// Register `crap.globals.get(slug, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_globals_get(
    lua: &Lua,
    table: &mlua::Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let get_fn = lua.create_function(move |lua, (slug, opts): (String, Option<mlua::Table>)| {
        let conn_ptr = get_tx_conn(lua)?;
        let conn = unsafe { &*conn_ptr };

        let locale_str: Option<String> = opts
            .as_ref()
            .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
        let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

        let def = {
            let r = reg
                .read()
                .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock: {}", e)))?;
            r.get_global(&slug)
                .cloned()
                .ok_or_else(|| mlua::Error::RuntimeError(format!("Global '{}' not found", slug)))?
        };

        let doc = query::get_global(conn, &slug, &def, locale_ctx.as_ref())
            .map_err(|e| mlua::Error::RuntimeError(format!("get_global error: {}", e)))?;

        document_to_lua_table(lua, &doc)
    })?;
    table.set("get", get_fn)?;
    Ok(())
}

/// Register `crap.globals.update(slug, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_globals_update(
    lua: &Lua,
    table: &mlua::Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let update_fn = lua.create_function(
        move |lua, (slug, data_table, opts): (String, mlua::Table, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let locale_str: Option<String> = opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
            let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

            let def = {
                let r = reg
                    .read()
                    .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock: {}", e)))?;
                r.get_global(&slug).cloned().ok_or_else(|| {
                    mlua::Error::RuntimeError(format!("Global '{}' not found", slug))
                })?
            };

            let data = lua_table_to_hashmap(&data_table)?;
            let doc = query::update_global(conn, &slug, &def, &data, locale_ctx.as_ref())
                .map_err(|e| mlua::Error::RuntimeError(format!("update_global error: {}", e)))?;

            document_to_lua_table(lua, &doc)
        },
    )?;
    table.set("update", update_fn)?;
    Ok(())
}

/// Register `crap.jobs.queue(slug, data?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_jobs_queue(
    lua: &Lua,
    table: &mlua::Table,
    registry: SharedRegistry,
) -> Result<()> {
    let reg = registry;
    let queue_fn =
        lua.create_function(move |lua, (slug, data): (String, Option<mlua::Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            // Verify job exists in registry
            let job_def = {
                let r = reg
                    .read()
                    .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock: {}", e)))?;
                r.get_job(&slug).cloned().ok_or_else(|| {
                    mlua::Error::RuntimeError(format!("Job '{}' not defined", slug))
                })?
            };

            let data_json = match data {
                Some(tbl) => {
                    let json_val = crate::hooks::api::lua_to_json(lua, &Value::Table(tbl))?;
                    serde_json::to_string(&json_val)
                        .map_err(|e| mlua::Error::RuntimeError(format!("JSON error: {}", e)))?
                }
                None => "{}".to_string(),
            };

            let job_run = crate::db::query::jobs::insert_job(
                conn,
                &slug,
                &data_json,
                "hook",
                job_def.retries + 1,
                &job_def.queue,
            )
            .map_err(|e| mlua::Error::RuntimeError(format!("queue error: {}", e)))?;

            Ok(job_run.id)
        })?;
    table.set("queue", queue_fn)?;
    Ok(())
}
