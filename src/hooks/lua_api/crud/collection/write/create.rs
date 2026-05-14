//! Registration of `crap.collections.create` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    config::LocaleConfig,
    core::Registry,
    db::LocaleContext,
    hooks::{
        lifecycle::converters::document_to_lua_table,
        lua_api::crud::{
            get_tx_conn,
            helpers::{
                ExtractedData, check_hook_depth, extract_data, get_opt_bool, get_opt_string,
                hook_lua_infra, hook_ui_locale, hook_user, resolve_collection,
            },
        },
    },
    service::{LuaWriteHooks, ServiceContext, WriteInput, create_document},
};

/// Execute the `crap.collections.create` operation.
fn create_document_lua(
    lua: &Lua,
    reg: &Registry,
    lc: &LocaleConfig,
    collection: &str,
    data_table: &Table,
    opts: Option<&Table>,
) -> mlua::Result<Table> {
    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let lua_infra = hook_lua_infra(lua);
    let locale_str = get_opt_string(opts, "locale");
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc)
        .map_err(|e| RuntimeError(e.to_string()))?;
    let override_access = get_opt_bool(opts, "overrideAccess", false);
    let run_hooks = get_opt_bool(opts, "hooks", true);
    let draft = get_opt_bool(opts, "draft", false);
    let def = resolve_collection(reg, collection)?;

    let ExtractedData { data, password } = extract_data(data_table, &def)?;

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, collection, "create");

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(override_access)
        .registry(Some(reg))
        .hooks_enabled(hooks_enabled)
        .build();

    let write_input = WriteInput::builder(data)
        .password(password.as_deref())
        .locale_ctx(locale_ctx.as_ref())
        .locale(locale_str)
        .draft(draft)
        .ui_locale(ui_locale.clone())
        .build();

    let ctx = ServiceContext::collection(collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(override_access)
        .lua_infra(lua_infra.as_ref())
        .build();

    let (doc, _) = create_document(&ctx, write_input)
        .map_err(|e| RuntimeError(format!("create error: {e:#}")))?;

    document_to_lua_table(lua, &doc)
}

/// Register `crap.collections.create(collection, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_create(
    lua: &Lua,
    table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let create_fn = lua.create_function(
        move |lua, (collection, data_table, opts): (String, mlua::Table, Option<mlua::Table>)| {
            create_document_lua(lua, &registry, &lc, &collection, &data_table, opts.as_ref())
        },
    )?;

    table.set("create", create_fn)?;

    Ok(())
}
