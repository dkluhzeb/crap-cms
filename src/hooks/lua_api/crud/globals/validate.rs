//! Registration of `crap.globals.validate` Lua function.

use std::sync::Arc;

use crate::hooks::lua_api::utils::lua_err;
use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::{
    config::LocaleConfig,
    core::{DocumentFields, Registry},
    db::{LocaleContext, query::helpers::global_table},
    hooks::{
        lifecycle::converters::{lua_table_to_hashmap, lua_table_to_json_map},
        lua_api::crud::{
            collection::write::validate::ValidateResult,
            get_tx_conn,
            helpers::{hook_ui_locale, hook_user, resolve_global},
        },
    },
    service::{
        LuaWriteHooks, ServiceError, ValidateContext, WriteInput, validate_document,
        values_from_strings,
    },
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Optional options for `crap.globals.validate`. Globals are a singleton
/// row, so there is no `id` to exclude — validation always runs in update
/// mode against the fixed `default` row.
#[derive(Default, Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.GlobalValidateOptions")]
pub(crate) struct GlobalValidateOptions {
    /// Locale code for localized fields. Nil = default locale.
    pub(crate) locale: Option<String>,
    /// Skip access control checks (default: `false`).
    #[lua(optional)]
    pub(crate) override_access: bool,
    /// Validate as a draft (relaxes required-field checks for globals with
    /// drafts enabled).
    #[lua(optional)]
    pub(crate) draft: bool,
}

impl FromLua for GlobalValidateOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// State threaded into `crap.globals.validate`.
pub(crate) struct GlobalsValidateState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
}

/// Validate global field data without persisting. Returns
/// `{ valid = true }` on success, or `{ valid = false, errors = {...} }`
/// on validation failure (errors keyed by field name). Globals always
/// validate in update mode against the singleton `default` row.
#[lua_fn(
    path = "crap.globals.validate",
    returns = "crap.ValidateResult",
    auto_tx
)]
fn globals_validate(
    state: &GlobalsValidateState,
    lua: &Lua,
    #[lua(doc = "Global slug.")] slug: String,
    #[lua(ty = "table<string, any>", doc = "Field values to validate.")] data_table: Table,
    #[lua(ty = "crap.GlobalValidateOptions", doc = "Optional options.")] opts: Option<
        GlobalValidateOptions,
    >,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();
    let reg = &state.registry;
    let lc = &state.locale_config;

    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let locale_ctx =
        LocaleContext::from_locale_string(opts.locale.as_deref(), lc).map_err(lua_err)?;
    let def = resolve_global(reg, &slug)?;

    let mut data = values_from_strings(lua_table_to_hashmap(&data_table)?);
    let composite_data: DocumentFields = lua_table_to_json_map(&data_table)?
        .into_iter()
        .filter(|(_, v)| !matches!(v, JsonValue::String(_)))
        .collect();
    data.extend(composite_data);

    let write_hooks = LuaWriteHooks::builder(lua)
        .override_access(opts.override_access)
        .registry(Some(reg.as_ref()))
        .build();

    let table = global_table(&slug);
    let validate_ctx = ValidateContext {
        slug: &slug,
        table_name: &table,
        fields: &def.fields,
        hooks: &def.hooks,
        operation: "update",
        exclude_id: Some("default"),
        soft_delete: false,
        supports_drafts: def.has_drafts(),
        required_locales: None,
    };

    let input = WriteInput::builder(data)
        .locale_ctx(locale_ctx.as_ref())
        .draft(opts.draft)
        .ui_locale(ui_locale.clone())
        .build();

    let result = match validate_document(conn, &write_hooks, &validate_ctx, input, user.as_ref()) {
        Ok(()) => ValidateResult {
            valid: true,
            errors: None,
        },
        Err(ServiceError::Validation(ve)) => {
            let errors = ve
                .errors
                .iter()
                .map(|fe| (fe.field.clone(), fe.message.clone()))
                .collect();
            ValidateResult {
                valid: false,
                errors: Some(errors),
            }
        }
        Err(e) => return Err(RuntimeError(format!("validate error: {e}"))),
    };

    let value = lua.to_value(&result)?;
    let Value::Table(tbl) = value else {
        return Err(RuntimeError(
            "ValidateResult did not serialize to a table".into(),
        ));
    };
    Ok(tbl)
}

lua_table! {
    name: crap_globals_validate,
    path: "crap.globals",
    state: GlobalsValidateState,
    fns: [globals_validate],
}

/// Register `crap.globals.validate(slug, data, opts?)`. Parent
/// `crap.globals` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_globals_validate(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    register_crap_globals_validate(
        lua,
        GlobalsValidateState {
            registry,
            locale_config: locale_config.clone(),
        },
    )?;
    Ok(())
}
