//! Registration of `crap.collections.restore` Lua function.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{core::SharedRegistry, db::query};

use super::{get_tx_conn, helpers::*};

/// Restore a soft-deleted document by ID.
///
/// Validates that the collection supports soft delete, checks trash access,
/// restores the document, and re-syncs the FTS index.
fn restore_document(
    lua: &Lua,
    reg: &SharedRegistry,
    collection: &str,
    id: &str,
    opts: &Option<Table>,
) -> mlua::Result<bool> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let override_access = get_opt_bool(opts, "overrideAccess", false)?;
    let def = resolve_collection(reg, collection)?;

    if !def.soft_delete {
        return Err(RuntimeError(format!(
            "Collection '{}' does not have soft_delete enabled",
            collection
        )));
    }

    enforce_access(
        lua,
        override_access,
        def.access.resolve_trash(),
        Some(id),
        &mut vec![],
        "Restore access denied",
    )?;

    let restored = query::restore(conn, collection, id)
        .map_err(|e| RuntimeError(format!("restore error: {e:#}")))?;

    if !restored {
        return Err(RuntimeError(format!(
            "Document '{}' not found or not deleted in '{}'",
            id, collection
        )));
    }

    // Re-sync FTS index
    if conn.supports_fts()
        && let Ok(Some(doc)) = query::find_by_id_unfiltered(conn, collection, &def, id, None)
    {
        query::fts::fts_upsert(conn, collection, &doc, Some(&def))
            .map_err(|e| RuntimeError(format!("FTS upsert error: {e:#}")))?;
    }

    Ok(true)
}

/// Register `crap.collections.restore(collection, id, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_restore(lua: &Lua, table: &Table, registry: SharedRegistry) -> Result<()> {
    let restore_fn = lua.create_function(
        move |lua, (collection, id, opts): (String, String, Option<Table>)| {
            restore_document(lua, &registry, &collection, &id, &opts)
        },
    )?;
    table.set("restore", restore_fn)?;

    Ok(())
}
