//! Registration of `crap.collections.validate` Lua function.

use std::collections::HashMap;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    db::LocaleContext,
    hooks::lifecycle::{
        converters::{flatten_lua_groups, lua_table_to_hashmap, lua_table_to_json_map},
        crud::{get_tx_conn, helpers::*},
    },
    service::{LuaWriteHooks, ServiceError, ValidateContext, WriteInput, validate_document},
};

/// Core logic for `crap.collections.validate`.
fn validate_inner(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    collection: String,
    data_table: Table,
    opts: Option<Table>,
) -> LuaResult<Table> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let locale_str = get_opt_string(&opts, "locale")?;
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc);
    let override_access = get_opt_bool(&opts, "overrideAccess", false)?;
    let draft = get_opt_bool(&opts, "draft", false)?;
    let exclude_id = get_opt_string(&opts, "id")?;
    let def = resolve_collection(reg, &collection)?;

    let mut data = lua_table_to_hashmap(&data_table)?;
    flatten_lua_groups(&data_table, &def.fields, &mut data)?;

    let password = if def.is_auth_collection() {
        data.remove("password")
    } else {
        None
    };

    let join_data: HashMap<String, Value> = lua_table_to_json_map(lua, &data_table)?
        .into_iter()
        .filter(|(_, v)| !matches!(v, Value::String(_)))
        .collect();

    let r = reg
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(override_access)
        .registry(Some(&r))
        .build();

    let operation = if exclude_id.is_some() {
        "update"
    } else {
        "create"
    };

    let validate_ctx = ValidateContext {
        slug: &collection,
        table_name: &collection,
        fields: &def.fields,
        hooks: &def.hooks,
        operation,
        exclude_id: exclude_id.as_deref(),
        soft_delete: def.has_soft_delete(),
    };

    let input = WriteInput::builder(data, &join_data)
        .password(password.as_deref())
        .locale_ctx(locale_ctx.as_ref())
        .locale(locale_str)
        .draft(draft)
        .ui_locale(ui_locale.clone())
        .build();

    let result_table = lua.create_table()?;

    match validate_document(conn, &write_hooks, &validate_ctx, input, user.as_ref()) {
        Ok(()) => {
            result_table.set("valid", true)?;
        }
        Err(ServiceError::Validation(ve)) => {
            result_table.set("valid", false)?;

            let errors = lua.create_table()?;
            for fe in &ve.errors {
                errors.set(fe.field.as_str(), fe.message.as_str())?;
            }
            result_table.set("errors", errors)?;
        }
        Err(e) => return Err(RuntimeError(format!("validate error: {e}"))),
    }

    Ok(result_table)
}

/// Register `crap.collections.validate(collection, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_validate(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let validate_fn = lua.create_function(
        move |lua, (collection, data_table, opts): (String, Table, Option<Table>)| {
            validate_inner(lua, &registry, &lc, collection, data_table, opts)
        },
    )?;

    table.set("validate", validate_fn)?;

    Ok(())
}
