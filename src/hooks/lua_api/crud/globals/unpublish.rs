//! Registration of `crap.globals.unpublish` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{
    Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value as LuaValue,
};
use serde::Deserialize;

use crate::config::LocaleConfig;
use crate::core::Registry;
use crate::hooks::lifecycle::converters::document_to_lua_table;
use crate::hooks::lua_api::crud::{
    get_tx_conn,
    helpers::{check_hook_depth, hook_lua_infra, hook_ui_locale, hook_user, resolve_global},
};
use crate::service::{
    LuaWriteHooks, ServiceContext,
    op::{Operation, UnpublishGlobal, UnpublishGlobalArgs},
};
use crate::typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Optional options for `crap.globals.unpublish`.
#[derive(Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.GlobalUnpublishOptions")]
pub(crate) struct GlobalUnpublishOptions {
    /// Skip access control checks (default: `false`).
    #[lua(optional)]
    pub(crate) override_access: bool,
    /// Run lifecycle hooks (default: `true`).
    #[lua(optional)]
    pub(crate) hooks: bool,
}

impl Default for GlobalUnpublishOptions {
    fn default() -> Self {
        Self {
            override_access: false,
            hooks: true,
        }
    }
}

impl FromLua for GlobalUnpublishOptions {
    fn from_lua(value: LuaValue, lua: &Lua) -> LuaResult<Self> {
        match value {
            LuaValue::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// State threaded into `crap.globals.unpublish`.
pub(crate) struct GlobalsUnpublishState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
}

/// Unpublish a global — sets `_status` to `"draft"` without modifying the
/// stored field data. Only available on globals with `versions` enabled.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(path = "crap.globals.unpublish", returns = "crap.Document", auto_tx)]
fn globals_unpublish(
    state: &GlobalsUnpublishState,
    lua: &Lua,
    #[lua(doc = "Global slug.")] slug: String,
    #[lua(ty = "crap.GlobalUnpublishOptions", doc = "Optional options.")] opts: Option<
        GlobalUnpublishOptions,
    >,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();
    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let lua_infra = hook_lua_infra(lua);
    let def = resolve_global(&state.registry, &slug)?;

    let (hooks_enabled, _guard) = check_hook_depth(lua, opts.hooks, &slug, "update");

    let write_hooks = LuaWriteHooks::builder(lua)
        .override_access(opts.override_access)
        .registry(Some(state.registry.as_ref()))
        .hooks_enabled(hooks_enabled)
        .build();

    let ctx = ServiceContext::global(&slug, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .ui_locale(ui_locale.clone())
        .override_access(opts.override_access)
        .locale_config(Some(&state.locale_config))
        .lua_infra(lua_infra.as_ref())
        .build();

    // Shared operation body — identical semantics on every surface.
    let doc = UnpublishGlobal::run(&ctx, UnpublishGlobalArgs::default())
        .map_err(|e| RuntimeError(format!("unpublish global error: {e:#}")))?;

    document_to_lua_table(lua, &doc)
}

lua_table! {
    name: crap_globals_unpublish,
    path: "crap.globals",
    state: GlobalsUnpublishState,
    fns: [globals_unpublish],
}

/// Register `crap.globals.unpublish(slug, opts?)`. Parent `crap.globals`
/// must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_globals_unpublish(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    register_crap_globals_unpublish(
        lua,
        GlobalsUnpublishState {
            registry,
            locale_config: locale_config.clone(),
        },
    )?;
    Ok(())
}
