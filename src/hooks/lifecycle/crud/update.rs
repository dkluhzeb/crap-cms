//! Registration of `crap.collections.update` Lua function.

use std::collections::HashMap;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    db::{LocaleContext, query},
    hooks::lifecycle::{
        access::{check_field_read_access_with_lua, check_field_write_access_with_lua},
        converters::*,
    },
    service::{LuaWriteHooks, WriteInput, update_document_core},
};

use super::{get_tx_conn, helpers::*};
use super::unpublish::{UnpublishCtx, handle_unpublish};

/// Execute the `crap.collections.update` operation.
fn update_document(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    collection: String,
    id: String,
    data_table: Table,
    opts: Option<Table>,
) -> mlua::Result<Table> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let locale_str = get_opt_string(&opts, "locale")?;
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc);
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;
    let run_hooks = get_opt_bool(&opts, "hooks", true)?;
    let unpublish = get_opt_bool(&opts, "unpublish", false)?;
    let draft = get_opt_bool(&opts, "draft", false)?;
    let def = resolve_collection(reg, &collection)?;

    enforce_access(
        lua, override_access, def.access.update.as_deref(),
        Some(&id), &mut vec![], "Update access denied",
    )?;

    // Handle unpublish early return
    if unpublish && def.has_versions() {
        return handle_unpublish(
            lua, conn,
            &UnpublishCtx::builder(&collection, &id, &def)
                .run_hooks(run_hooks)
                .locale_str(locale_str.as_deref())
                .hook_user(user.as_ref())
                .hook_ui_locale(ui_locale.as_deref())
                .build(),
        );
    }

    let ExtractedData { mut flat, mut hook, password } = extract_data(lua, &data_table, &def)?;

    // Strip write-denied fields
    if !override_access {
        let denied = check_field_write_access_with_lua(lua, &def.fields, user.as_ref(), "update");
        for name in &denied {
            flat.remove(name);
            hook.remove(name);
        }
    }

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, &collection, "update");

    // Separate join data from the merged hook map
    let join_data: HashMap<String, Value> = hook
        .iter()
        .filter(|(_, v)| !matches!(v, Value::String(_)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let r = reg.read().map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;
    let write_hooks = LuaWriteHooks {
        lua,
        user: user.as_ref(),
        ui_locale: ui_locale.as_deref(),
        override_access,
        registry: Some(&r),
        hooks_enabled,
        run_validation: run_hooks,
    };

    let write_input = WriteInput::builder(flat, &join_data)
        .password(password.as_deref())
        .locale_ctx(locale_ctx.as_ref())
        .locale(locale_str)
        .draft(draft)
        .ui_locale(ui_locale.clone())
        .build();

    let (mut doc, _ctx) = update_document_core(conn, &write_hooks, &collection, &id, &def, write_input, user.as_ref())
        .map_err(|e| RuntimeError(format!("update error: {e:#}")))?;

    // Hydrate join fields and strip read-denied fields before returning
    query::hydrate_document(conn, &collection, &def.fields, &mut doc, None, locale_ctx.as_ref())
        .map_err(|e| RuntimeError(format!("hydrate error: {e:#}")))?;

    if !override_access {
        let denied = check_field_read_access_with_lua(lua, &def.fields, user.as_ref());
        for name in &denied {
            doc.fields.remove(name);
        }
    }

    document_to_lua_table(lua, &doc)
}

/// Register `crap.collections.update(collection, id, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(super) fn register_update(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let update_fn = lua.create_function(
    move |lua, (collection, id, data_table, opts): (String, String, Table, Option<Table>)| {
            update_document(lua, &registry, &lc, collection, id, data_table, opts)
        },
    )?;

    table.set("update", update_fn)?;
    Ok(())
}
