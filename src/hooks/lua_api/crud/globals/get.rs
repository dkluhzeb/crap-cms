//! Registration of `crap.globals.get` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;

use crate::config::LocaleConfig;
use crate::core::Registry;
use crate::db::LocaleContext;
use crate::hooks::lifecycle::converters::document_to_lua_table;
use crate::hooks::lua_api::crud::{
    get_tx_conn,
    helpers::{check_hook_depth, hook_ui_locale, hook_user, resolve_global},
};
use crate::service::{GetGlobalInput, LuaReadHooks, ServiceContext, get_global_document};
use crate::typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Optional options for `crap.globals.get`.
#[derive(Default, Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.GlobalGetOptions")]
pub(crate) struct GlobalGetOptions {
    /// Locale code for localized fields. Nil = default locale.
    pub(crate) locale: Option<String>,
    /// Skip access control checks (default: `false`). Set to `true` in
    /// trusted internal code to bypass the global's read access function.
    #[lua(optional)]
    pub(crate) override_access: bool,
    /// Include unpublished (draft) content (default: `false`). When the global
    /// has drafts enabled and has been unpublished, a normal read serves the
    /// last published snapshot; set this to `true` to read the draft instead.
    #[lua(optional)]
    pub(crate) draft: bool,
}

impl FromLua for GlobalGetOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// State threaded into `crap.globals.get` — the snapshot registry plus
/// the locale config (cloned once at registration time).
pub(crate) struct GlobalsGetState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
}

/// Get a global's current value.
#[lua_fn(path = "crap.globals.get", returns = "crap.Document", auto_tx)]
fn globals_get(
    state: &GlobalsGetState,
    lua: &Lua,
    #[lua(doc = "Global slug.")] slug: String,
    #[lua(
        ty = "crap.GlobalGetOptions",
        doc = "Optional options (e.g., `{ locale = \"de\" }`)."
    )]
    opts: Option<GlobalGetOptions>,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();
    let conn = get_tx_conn(lua)?;

    let locale_ctx =
        LocaleContext::from_locale_string(opts.locale.as_deref(), &state.locale_config)
            .map_err(|e| RuntimeError(e.to_string()))?;
    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let def = resolve_global(&state.registry, &slug)?;

    // Depth guard: a before_read/after_read hook that reads the same
    // global recurses — cap it like the write paths do.
    let (hooks_enabled, _guard) = check_hook_depth(lua, true, &slug, "get_global");

    let hooks = LuaReadHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(opts.override_access)
        .hooks_enabled(hooks_enabled)
        .build();

    let ctx = ServiceContext::global(&slug, &def)
        .conn(conn)
        .read_hooks(&hooks)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .build();

    let input =
        GetGlobalInput::new(locale_ctx.as_ref(), ui_locale.as_deref()).include_drafts(opts.draft);

    let doc = get_global_document(&ctx, &input).map_err(|e| RuntimeError(format!("{e}")))?;

    document_to_lua_table(lua, &doc)
}

lua_table! {
    name: crap_globals_get,
    path: "crap.globals",
    state: GlobalsGetState,
    fns: [globals_get],
}

/// Register `crap.globals.get(slug, opts?)`. Parent `crap.globals` must
/// already exist (populated by `register_globals_init` or
/// `register_globals_pool_init`).
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_globals_get(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    register_crap_globals_get(
        lua,
        GlobalsGetState {
            registry,
            locale_config: locale_config.clone(),
        },
    )?;
    Ok(())
}
