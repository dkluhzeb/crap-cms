//! Registration of `crap.collections.create_many` Lua function.

use std::collections::HashMap;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table, Value};
use serde_json::Value as JsonValue;

use crate::{
    core::SharedRegistry,
    hooks::lifecycle::{
        converters::{document_to_lua_table, lua_table_to_hashmap, lua_table_to_json_map},
        crud::{get_tx_conn, helpers::*},
    },
    service::{self, CreateManyItem, CreateManyOptions, LuaWriteHooks, ServiceContext},
};

/// Parse a single Lua table item into a `CreateManyItem`.
fn parse_item(lua: &Lua, item_table: &Table) -> mlua::Result<CreateManyItem> {
    let data = lua_table_to_hashmap(item_table)?;

    let join_data: HashMap<String, JsonValue> = lua_table_to_json_map(lua, item_table)?
        .into_iter()
        .filter(|(_, v)| !matches!(v, JsonValue::String(_)))
        .collect();

    let password = data.get("password").cloned();

    let mut data = data;
    data.remove("password");

    Ok(CreateManyItem {
        data,
        join_data,
        password,
    })
}

/// Bulk create multiple documents from an array of data tables.
///
/// Delegates to `service::create_many` which handles the full per-document
/// lifecycle: field hooks, validation, before/after change hooks, DB write,
/// ref count updates, FTS sync, and version snapshots.
fn create_many_documents(
    lua: &Lua,
    reg: &SharedRegistry,
    collection: &str,
    items_table: &Table,
    opts: &Option<Table>,
) -> mlua::Result<Table> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let override_access = get_opt_bool(opts, "overrideAccess", false)?;
    let run_hooks = get_opt_bool(opts, "hooks", true)?;
    let draft = get_opt_bool(opts, "draft", false)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let lua_infra = hook_lua_infra(lua);
    let def = resolve_collection(reg, collection)?;

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, collection, "create_many");

    // Parse items from the Lua array table
    let mut items = Vec::new();
    for i in 1..=items_table.raw_len() {
        let item_val: Value = items_table.raw_get(i)?;

        let Value::Table(item_table) = item_val else {
            return Err(RuntimeError(format!(
                "create_many: item at index {i} is not a table"
            )));
        };

        items.push(parse_item(lua, &item_table)?);
    }

    let r = reg
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(override_access)
        .registry(Some(&r))
        .hooks_enabled(hooks_enabled)
        .run_validation(run_hooks)
        .build();

    let mut ctx_builder = ServiceContext::collection(collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(override_access);

    if let Some(ref infra) = lua_infra {
        ctx_builder = ctx_builder.lua_infra(infra);
    }

    let ctx = ctx_builder.build();

    let create_opts = CreateManyOptions {
        run_hooks: hooks_enabled,
        draft,
    };

    let svc_result = service::create_many(&ctx, items, &create_opts)
        .map_err(|e| RuntimeError(format!("{e:#}")))?;

    let result = lua.create_table()?;
    result.set("created", svc_result.created)?;

    let docs_table = lua.create_table()?;
    for (i, doc) in svc_result.documents.iter().enumerate() {
        let doc_table = document_to_lua_table(lua, doc)?;
        docs_table.raw_set(i + 1, doc_table)?;
    }
    result.set("documents", docs_table)?;

    Ok(result)
}

/// Register `crap.collections.create_many(collection, items, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_create_many(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
) -> Result<()> {
    let create_many_fn = lua.create_function(
        move |lua, (collection, items_table, opts): (String, Table, Option<Table>)| {
            create_many_documents(lua, &registry, &collection, &items_table, &opts)
        },
    )?;

    table.set("create_many", create_many_fn)?;

    Ok(())
}
