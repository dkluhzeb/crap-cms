//! Registration of `crap.globals.update` Lua function.

use std::sync::Arc;

use crate::hooks::lua_api::utils::lua_err;
use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::config::LocaleConfig;
use crate::core::{DocumentFields, Registry};
use crate::db::LocaleContext;
use crate::hooks::lifecycle::converters::{
    document_to_lua_table, lua_table_to_hashmap, lua_table_to_json_map,
};
use crate::hooks::lua_api::crud::{
    get_tx_conn,
    helpers::{check_hook_depth, hook_lua_infra, hook_ui_locale, hook_user, resolve_global},
};
use crate::service::{
    LuaWriteHooks, ServiceContext, WriteInput, update_global_document, values_from_strings,
};
use crate::typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Optional options for `crap.globals.update`.
#[derive(Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.GlobalUpdateOptions")]
pub(crate) struct GlobalUpdateOptions {
    /// Locale code for localized fields. Nil = default locale.
    pub(crate) locale: Option<String>,
    /// Skip access control checks (default: `false`). Set to `true` in
    /// trusted internal code to bypass the global's update access function.
    #[lua(optional)]
    pub(crate) override_access: bool,
    /// Run lifecycle hooks (default: `true`). Set false to bypass hooks
    /// (e.g., for seeding/migrations).
    #[lua(optional)]
    pub(crate) hooks: bool,
    /// When `true` and the global has `versions.drafts`, performs a
    /// version-only save (main row unchanged, only a draft version
    /// snapshot is created). Matches `crap.collections.update`'s `draft`.
    #[lua(optional)]
    pub(crate) draft: bool,
    /// Emit a live-update event for the updated global (default: `true`).
    /// Set `false` for a quiet write.
    #[lua(optional)]
    pub(crate) events: bool,
}

impl Default for GlobalUpdateOptions {
    fn default() -> Self {
        Self {
            locale: None,
            override_access: false,
            hooks: true,
            draft: false,
            events: true,
        }
    }
}

impl FromLua for GlobalUpdateOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// State threaded into `crap.globals.update`.
pub(crate) struct GlobalsUpdateState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
}

/// Update a global's value.
#[lua_fn(path = "crap.globals.update", returns = "crap.Document", auto_tx)]
fn globals_update(
    state: &GlobalsUpdateState,
    lua: &Lua,
    #[lua(doc = "Global slug.")] slug: String,
    #[lua(ty = "table<string, any>", doc = "Fields to update.")] data_table: Table,
    #[lua(
        ty = "crap.GlobalUpdateOptions",
        doc = "Optional options (e.g., `{ locale = \"de\" }`)."
    )]
    opts: Option<GlobalUpdateOptions>,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();
    let reg = &state.registry;
    let lc = &state.locale_config;

    let conn = get_tx_conn(lua)?;

    let lua_infra = hook_lua_infra(lua);
    let locale_ctx =
        LocaleContext::from_locale_string(opts.locale.as_deref(), lc).map_err(lua_err)?;
    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let def = resolve_global(reg, &slug)?;

    let mut data = values_from_strings(lua_table_to_hashmap(&data_table)?);
    let composite_data: DocumentFields = lua_table_to_json_map(&data_table)?
        .into_iter()
        .filter(|(_, v)| !matches!(v, JsonValue::String(_)))
        .collect();
    data.extend(composite_data);

    let (hooks_enabled, _guard) = check_hook_depth(lua, opts.hooks, &slug, "update");

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(opts.override_access)
        .registry(Some(reg.as_ref()))
        .hooks_enabled(hooks_enabled)
        .build();

    let write_input = WriteInput::builder(data)
        .locale_ctx(locale_ctx.as_ref())
        .locale(opts.locale)
        .draft(opts.draft)
        .ui_locale(ui_locale.clone())
        .build();

    let ctx = ServiceContext::global(&slug, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .emit_events(opts.events)
        .lua_infra(lua_infra.as_ref())
        .build();

    let (doc, _) = update_global_document(&ctx, write_input)
        .map_err(|e| RuntimeError(format!("update_global error: {e:#}")))?;

    document_to_lua_table(lua, &doc)
}

lua_table! {
    name: crap_globals_update,
    path: "crap.globals",
    state: GlobalsUpdateState,
    fns: [globals_update],
}

/// Register `crap.globals.update(slug, data, opts?)`. Parent `crap.globals`
/// must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_globals_update(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    register_crap_globals_update(
        lua,
        GlobalsUpdateState {
            registry,
            locale_config: locale_config.clone(),
        },
    )?;
    Ok(())
}
