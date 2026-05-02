//! Registration of `crap.collections.update` Lua function.

use std::collections::HashMap;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};
use serde_json::Value;

use super::unpublish::{UnpublishCtx, handle_unpublish};

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    db::LocaleContext,
    hooks::lifecycle::{
        converters::*,
        crud::{get_tx_conn, helpers::*},
    },
    service::{LuaWriteHooks, ServiceContext, WriteInput, update_document},
};

/// Execute the `crap.collections.update` operation.
fn update_document_lua(
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
    let lua_infra = hook_lua_infra(lua);
    let locale_str = get_opt_string(&opts, "locale")?;
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc)
        .map_err(|e| RuntimeError(e.to_string()))?;
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;
    let run_hooks = get_opt_bool(&opts, "hooks", true)?;
    let unpublish = get_opt_bool(&opts, "unpublish", false)?;
    let draft = get_opt_bool(&opts, "draft", false)?;
    let def = resolve_collection(reg, &collection)?;

    // Collection-level access check is handled inside service::update_document
    // via WriteHooks::check_access (respects override_access on LuaWriteHooks).

    // Handle unpublish early return
    if unpublish && def.has_versions() {
        return handle_unpublish(
            lua,
            conn,
            &UnpublishCtx::builder(&collection, &id, &def)
                .run_hooks(run_hooks)
                .locale_str(locale_str.as_deref())
                .hook_user(user.as_ref())
                .hook_ui_locale(ui_locale.as_deref())
                .build(),
        );
    }

    let ExtractedData {
        flat,
        hook,
        password,
    } = extract_data(lua, &data_table, &def)?;

    // Field write access is now checked inside service::update_document
    // via WriteHooks::field_write_denied.

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, &collection, "update");

    // Separate join data from the merged hook map
    let join_data: HashMap<String, Value> = hook
        .iter()
        .filter(|(_, v)| !matches!(v, Value::String(_)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let r = reg
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(override_access)
        .registry(Some(&r))
        .hooks_enabled(hooks_enabled)
        .build();

    let write_input = WriteInput::builder(flat, &join_data)
        .password(password.as_deref())
        .locale_ctx(locale_ctx.as_ref())
        .locale(locale_str)
        .draft(draft)
        .ui_locale(ui_locale.clone())
        .build();

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(override_access)
        .lua_infra(lua_infra.as_ref())
        .build();

    let (doc, _) = update_document(&ctx, &id, write_input)
        .map_err(|e| RuntimeError(format!("update error: {e:#}")))?;

    // Hydration and read-denied field stripping are handled inside
    // update_document via WriteHooks.

    document_to_lua_table(lua, &doc)
}

/// Register `crap.collections.update(collection, id, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_update(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let update_fn = lua.create_function(
        move |lua, (collection, id, data_table, opts): (String, String, Table, Option<Table>)| {
            update_document_lua(lua, &registry, &lc, collection, id, data_table, opts)
        },
    )?;

    table.set("update", update_fn)?;
    Ok(())
}
