//! Registration of `crap.collections.create_many` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};
use serde_json::Value as JsonValue;

use crate::{
    core::{DocumentFields, Registry},
    hooks::{
        lifecycle::converters::{
            document_to_lua_table, lua_table_to_hashmap, lua_table_to_json_map,
        },
        lua_api::crud::{
            collection::write::create::CreateOptions,
            get_tx_conn,
            helpers::{
                check_hook_depth, hook_lua_infra, hook_ui_locale, hook_user, resolve_collection,
            },
        },
    },
    service::{self, CreateManyItem, CreateManyOptions, LuaWriteHooks, ServiceContext},
    typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Bulk create multiple documents from an array of data tables.
/// All-or-nothing: a single transaction wraps every insert.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.create_many",
    returns = "crap.CreateManyResult"
)]
fn collections_create_many(
    state: &Arc<Registry>,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(
        ty = "table<string, any>[]",
        doc = "Array of field-value tables — one per document."
    )]
    items: Table,
    #[lua(
        ty = "crap.CreateOptions",
        doc = "Optional options applied to every document."
    )]
    opts: Option<CreateOptions>,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();
    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let lua_infra = hook_lua_infra(lua);
    let def = resolve_collection(state, &collection)?;

    let (hooks_enabled, _guard) = check_hook_depth(lua, opts.hooks, &collection, "create_many");

    let mut parsed_items = Vec::new();
    for i in 1..=items.raw_len() {
        let item_val: Value = items.raw_get(i)?;

        let Value::Table(item_table) = item_val else {
            return Err(RuntimeError(format!(
                "create_many: item at index {i} is not a table"
            )));
        };

        parsed_items.push(parse_item(&item_table)?);
    }

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(opts.override_access)
        .registry(Some(state.as_ref()))
        .hooks_enabled(hooks_enabled)
        .run_validation(opts.hooks)
        .build();

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .lua_infra(lua_infra.as_ref())
        .build();

    let create_opts = CreateManyOptions {
        run_hooks: hooks_enabled,
        draft: opts.draft,
    };

    let svc_result = service::create_many(&ctx, &parsed_items, &create_opts)
        .map_err(|e| RuntimeError(format!("{e:#}")))?;

    let result = lua.create_table()?;
    result.set("created", svc_result.created)?;

    let docs_table = lua.create_table()?;
    for (i, doc) in svc_result.documents.iter().enumerate() {
        let entry = document_to_lua_table(lua, doc)?;
        docs_table.raw_set(i + 1, entry)?;
    }
    result.set("documents", docs_table)?;

    Ok(result)
}

lua_table! {
    name: crap_collections_create_many,
    path: "crap.collections",
    state: Arc<Registry>,
    fns: [collections_create_many],
}

/// Register `crap.collections.create_many(collection, items, opts?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_create_many(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
) -> Result<()> {
    register_crap_collections_create_many(lua, registry)?;
    Ok(())
}

/// Parse a single Lua table item into a `CreateManyItem`.
fn parse_item(item_table: &Table) -> LuaResult<CreateManyItem> {
    let mut data = service::values_from_strings(lua_table_to_hashmap(item_table)?);

    let composite_data: DocumentFields = lua_table_to_json_map(item_table)?
        .into_iter()
        .filter(|(_, v)| !matches!(v, JsonValue::String(_)))
        .collect();
    data.extend(composite_data);

    let password = data
        .remove("password")
        .and_then(|v| v.as_str().map(std::string::ToString::to_string));

    Ok(CreateManyItem { data, password })
}
