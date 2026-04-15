//! Registration of `crap.collections.list_versions` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};

use crate::{
    core::SharedRegistry,
    hooks::lifecycle::{
        converters::pagination_result_to_lua_table,
        crud::{get_tx_conn, helpers::*},
    },
    service::{ListVersionsInput, LuaReadHooks, ServiceContext, list_versions},
};

/// Core logic for `crap.collections.list_versions`.
fn list_versions_inner(
    lua: &Lua,
    reg: &SharedRegistry,
    collection: String,
    id: String,
    opts: Option<Table>,
) -> LuaResult<Table> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    // Validate collection exists
    let def = resolve_collection(reg, &collection)?;

    let limit: Option<i64> = opts
        .as_ref()
        .and_then(|o| o.get::<Option<i64>>("limit").ok().flatten());
    let offset: Option<i64> = opts
        .as_ref()
        .and_then(|o| o.get::<Option<i64>>("offset").ok().flatten());
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;

    let user = hook_user(lua);
    let hooks = LuaReadHooks::builder(lua)
        .user(user.as_ref())
        .override_access(override_access)
        .build();

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .read_hooks(&hooks)
        .user(user.as_ref())
        .override_access(override_access)
        .build();

    let input = ListVersionsInput::builder(&id)
        .limit(limit)
        .offset(offset)
        .build();

    let paginated = list_versions(&ctx, &input).map_err(|e| RuntimeError(format!("{e}")))?;

    let pagination = pagination_result_to_lua_table(lua, &paginated.pagination)?;

    let docs = lua.create_table()?;
    for (i, v) in paginated.docs.iter().enumerate() {
        let row = lua.create_table()?;
        row.set("id", v.id.as_str())?;
        row.set("version", v.version)?;
        row.set("status", v.status.as_str())?;
        row.set("latest", v.latest)?;

        if let Some(ref ts) = v.created_at {
            row.set("created_at", ts.as_str())?;
        }

        docs.set(i + 1, row)?;
    }

    let result = lua.create_table()?;
    result.set("docs", docs)?;
    result.set("pagination", pagination)?;

    Ok(result)
}

/// Register `crap.collections.list_versions(collection, id, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_list_versions(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
) -> Result<()> {
    let list_versions_fn = lua.create_function(
        move |lua, (collection, id, opts): (String, String, Option<Table>)| {
            list_versions_inner(lua, &registry, collection, id, opts)
        },
    )?;

    table.set("list_versions", list_versions_fn)?;

    Ok(())
}
