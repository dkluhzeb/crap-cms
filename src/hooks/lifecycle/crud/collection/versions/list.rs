//! Registration of `crap.collections.list_versions` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};

use crate::{core::SharedRegistry, service::list_versions};

use crate::hooks::lifecycle::crud::{get_tx_conn, helpers::*};

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
    let _def = resolve_collection(reg, &collection)?;

    let limit: Option<i64> = opts
        .as_ref()
        .and_then(|o| o.get::<Option<i64>>("limit").ok().flatten());
    let offset: Option<i64> = opts
        .as_ref()
        .and_then(|o| o.get::<Option<i64>>("offset").ok().flatten());

    let (versions, total) = list_versions(conn, &collection, &id, limit, offset)
        .map_err(|e| RuntimeError(format!("{e}")))?;

    let result = lua.create_table()?;
    result.set("total", total)?;

    let docs = lua.create_table()?;
    for (i, v) in versions.iter().enumerate() {
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

    result.set("docs", docs)?;

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
