//! Registration of `crap.globals.get`, `crap.globals.update`, and `crap.jobs.queue` Lua functions.

use std::collections::HashMap;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table, Value};

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    db::{AccessResult, LocaleContext, query},
    hooks::{
        ValidationCtx, api,
        lifecycle::{
            UserContext,
            access::{
                check_access_with_lua, check_field_read_access_with_lua,
                check_field_write_access_with_lua,
            },
            converters::*,
            validation::validate_fields_inner,
        },
    },
};

use super::get_tx_conn;

/// Register `crap.globals.get(slug, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_globals_get(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let get_fn = lua.create_function(move |lua, (slug, opts): (String, Option<Table>)| {
        let conn_ptr = get_tx_conn(lua)?;
        let conn = unsafe { &*conn_ptr };

        let locale_str: Option<String> = opts
            .as_ref()
            .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
        let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

        let override_access: bool = opts
            .as_ref()
            .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
            .unwrap_or(false);

        let def = {
            let r = reg
                .read()
                .map_err(|e| RuntimeError(format!("Registry lock: {:#}", e)))?;
            r.get_global(&slug)
                .cloned()
                .ok_or_else(|| RuntimeError(format!("Global '{}' not found", slug)))?
        };

        // Enforce collection-level read access
        if !override_access {
            let user_doc = lua
                .app_data_ref::<UserContext>()
                .and_then(|uc| uc.0.clone());
            let result = check_access_with_lua(
                lua,
                def.access.read.as_deref(),
                user_doc.as_ref(),
                None,
                None,
            )
            .map_err(|e| RuntimeError(format!("access check error: {:#}", e)))?;

            if matches!(result, AccessResult::Denied) {
                return Err(RuntimeError("Read access denied".into()));
            }
        }

        let mut doc = query::get_global(conn, &slug, &def, locale_ctx.as_ref())
            .map_err(|e| RuntimeError(format!("get_global error: {:#}", e)))?;

        // Strip field-level read-denied fields
        if !override_access {
            let user_doc = lua
                .app_data_ref::<UserContext>()
                .and_then(|uc| uc.0.clone());
            let denied = check_field_read_access_with_lua(lua, &def.fields, user_doc.as_ref());
            for name in &denied {
                doc.fields.remove(name);
            }
        }

        document_to_lua_table(lua, &doc)
    })?;
    table.set("get", get_fn)?;
    Ok(())
}

/// Register `crap.globals.update(slug, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_globals_update(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let reg = registry;
    let lc = locale_config.clone();
    let update_fn = lua.create_function(
        move |lua, (slug, data_table, opts): (String, Table, Option<Table>)| {
            let conn_ptr = get_tx_conn(lua)?;
            let conn = unsafe { &*conn_ptr };

            let locale_str: Option<String> = opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("locale").ok().flatten());
            let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), &lc);

            let override_access: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("overrideAccess").ok().flatten())
                .unwrap_or(false);

            let def = {
                let r = reg
                    .read()
                    .map_err(|e| RuntimeError(format!("Registry lock: {:#}", e)))?;
                r.get_global(&slug)
                    .cloned()
                    .ok_or_else(|| RuntimeError(format!("Global '{}' not found", slug)))?
            };

            // Enforce collection-level update access
            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let result = check_access_with_lua(
                    lua,
                    def.access.update.as_deref(),
                    user_doc.as_ref(),
                    None,
                    None,
                )
                .map_err(|e| RuntimeError(format!("access check error: {:#}", e)))?;

                if matches!(result, AccessResult::Denied) {
                    return Err(RuntimeError("Update access denied".into()));
                }
            }

            let mut data = lua_table_to_hashmap(&data_table)?;
            let mut join_data = lua_table_to_json_map(lua, &data_table)?;

            // Strip field-level write-denied fields
            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let denied = check_field_write_access_with_lua(
                    lua,
                    &def.fields,
                    user_doc.as_ref(),
                    "update",
                );
                for name in &denied {
                    data.remove(name);
                    join_data.remove(name);
                }
            }

            // Build hook data for validation
            let mut hook_data: HashMap<String, serde_json::Value> = data
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect();
            for (k, v) in &join_data {
                hook_data.insert(k.clone(), v.clone());
            }

            // Run validation
            let global_table = format!("_global_{}", slug);
            let val_ctx = ValidationCtx::builder(conn, &global_table)
                .locale_ctx(locale_ctx.as_ref())
                .build();
            validate_fields_inner(lua, &def.fields, &hook_data, &val_ctx)
                .map_err(|e| RuntimeError(format!("validation error: {:#}", e)))?;

            let global_table = format!("_global_{}", slug);
            let old_refs = query::ref_count::snapshot_outgoing_refs(
                conn,
                &global_table,
                "default",
                &def.fields,
                &lc,
            )
            .map_err(|e| RuntimeError(format!("ref count snapshot error: {:#}", e)))?;

            query::update_global(conn, &slug, &def, &data, locale_ctx.as_ref())
                .map_err(|e| RuntimeError(format!("update_global error: {:#}", e)))?;

            query::save_join_table_data(
                conn,
                &global_table,
                &def.fields,
                "default",
                &join_data,
                locale_ctx.as_ref(),
            )
            .map_err(|e| RuntimeError(format!("join data error: {:#}", e)))?;

            query::ref_count::after_update(
                conn,
                &global_table,
                "default",
                &def.fields,
                &lc,
                old_refs,
            )
            .map_err(|e| RuntimeError(format!("ref count update error: {:#}", e)))?;

            // Re-fetch to hydrate join data in the returned document
            let mut doc = query::get_global(conn, &slug, &def, locale_ctx.as_ref())
                .map_err(|e| RuntimeError(format!("get_global error: {:#}", e)))?;

            // Strip field-level read-denied fields
            if !override_access {
                let user_doc = lua
                    .app_data_ref::<UserContext>()
                    .and_then(|uc| uc.0.clone());
                let denied = check_field_read_access_with_lua(lua, &def.fields, user_doc.as_ref());
                for name in &denied {
                    doc.fields.remove(name);
                }
            }

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
    table: &Table,
    registry: SharedRegistry,
) -> Result<()> {
    let reg = registry;
    let queue_fn = lua.create_function(move |lua, (slug, data): (String, Option<Table>)| {
        let conn_ptr = get_tx_conn(lua)?;
        let conn = unsafe { &*conn_ptr };

        // Verify job exists in registry
        let job_def = {
            let r = reg
                .read()
                .map_err(|e| RuntimeError(format!("Registry lock: {:#}", e)))?;
            r.get_job(&slug)
                .cloned()
                .ok_or_else(|| RuntimeError(format!("Job '{}' not defined", slug)))?
        };

        let data_json = match data {
            Some(tbl) => {
                let json_val = api::lua_to_json(lua, &Value::Table(tbl))?;
                serde_json::to_string(&json_val)
                    .map_err(|e| RuntimeError(format!("JSON error: {:#}", e)))?
            }
            None => "{}".to_string(),
        };

        let job_run = query::jobs::insert_job(
            conn,
            &slug,
            &data_json,
            "hook",
            job_def.retries + 1,
            &job_def.queue,
        )
        .map_err(|e| RuntimeError(format!("queue error: {:#}", e)))?;

        Ok(job_run.id)
    })?;
    table.set("queue", queue_fn)?;
    Ok(())
}
