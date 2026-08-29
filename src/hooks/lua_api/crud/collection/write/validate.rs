//! Registration of `crap.collections.validate` Lua function.

use std::sync::Arc;

use std::collections::HashMap;

use crate::hooks::lua_api::utils::lua_err;
use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::{Deserialize, Serialize};

use crate::{
    config::LocaleConfig,
    core::Registry,
    db::LocaleContext,
    hooks::lua_api::crud::{
        get_tx_conn,
        helpers::{ExtractedData, extract_data, hook_ui_locale, hook_user, resolve_collection},
    },
    service::{
        LuaWriteHooks, ServiceContext,
        op::{Operation, Validate, ValidateArgs},
    },
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Optional options for `crap.collections.validate`.
#[derive(Default, Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.ValidateOptions")]
pub(crate) struct ValidateOptions {
    /// Locale code for localized fields. Nil = default locale.
    pub(crate) locale: Option<String>,
    /// Skip access control checks (default: `false`).
    #[lua(optional)]
    pub(crate) override_access: bool,
    /// Validate as a draft (relaxes required-field checks the same way as
    /// `crap.collections.create({ draft = true })`).
    #[lua(optional)]
    pub(crate) draft: bool,
    /// Existing document ID to exclude from uniqueness checks (set when
    /// validating an update).
    pub(crate) id: Option<String>,
}

impl FromLua for ValidateOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// Result of `crap.collections.validate(...)`. `errors` is present only
/// when `valid = false`.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.ValidateResult")]
pub(crate) struct ValidateResult {
    /// True when the input passes validation.
    pub(crate) valid: bool,
    /// Map of field name → error message. Present only when
    /// `valid = false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table<string, string>", optional)]
    pub(crate) errors: Option<HashMap<String, String>>,
}

/// State threaded into `crap.collections.validate`.
pub(crate) struct CollectionsValidateState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
}

/// Validate document data without persisting. Returns
/// `{ valid = true }` on success, or `{ valid = false, errors = {...} }`
/// on validation failure (errors keyed by field name).
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.validate",
    returns = "crap.ValidateResult",
    auto_tx
)]
fn collections_validate(
    state: &CollectionsValidateState,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(ty = "table<string, any>", doc = "Field values to validate.")] data: Table,
    #[lua(ty = "crap.ValidateOptions", doc = "Optional options.")] opts: Option<ValidateOptions>,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();
    let reg = &state.registry;
    let lc = &state.locale_config;

    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let locale_ctx =
        LocaleContext::from_locale_string(opts.locale.as_deref(), lc).map_err(lua_err)?;
    let def = resolve_collection(reg, &collection)?;

    // `password` is split off so it isn't validated as an unknown field; the
    // dry-run itself never touches it.
    let ExtractedData { data, password: _ } = extract_data(&data, &def)?;

    let write_hooks = LuaWriteHooks::builder(lua)
        .override_access(opts.override_access)
        .registry(Some(reg.as_ref()))
        .build();

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .ui_locale(ui_locale.clone())
        .override_access(opts.override_access)
        .build();

    // Shared operation body — identical dry-run semantics on every surface.
    let op_args = ValidateArgs::builder(data)
        .locale_ctx(locale_ctx)
        .exclude_id(opts.id.clone())
        .draft(opts.draft)
        .build();

    let outcome =
        Validate::run(&ctx, op_args).map_err(|e| RuntimeError(format!("validate error: {e}")))?;

    let result = ValidateResult {
        valid: outcome.is_none(),
        errors: outcome.map(|ve| ve.to_field_map()),
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
    name: crap_collections_validate,
    path: "crap.collections",
    state: CollectionsValidateState,
    fns: [collections_validate],
}

/// Register `crap.collections.validate(collection, data, opts?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_validate(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    register_crap_collections_validate(
        lua,
        CollectionsValidateState {
            registry,
            locale_config: locale_config.clone(),
        },
    )?;
    Ok(())
}
