//! Registration of `crap.globals.get` Lua function.

use std::sync::Arc;

use crate::hooks::lua_api::utils::lua_err;
use anyhow::Result;
use mlua::{FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;

use crate::config::{DepthConfig, LocaleConfig};
use crate::core::Registry;
use crate::db::{LocaleContext, query};
use crate::hooks::lifecycle::converters::document_to_lua_table;
use crate::hooks::lua_api::crud::{
    get_tx_conn,
    helpers::{check_hook_depth, hook_ui_locale, hook_user, resolve_global},
};
use crate::service::{
    LuaReadHooks, ServiceContext,
    op::{GetGlobal, GetGlobalArgs, Operation},
};
use crate::typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Optional options for `crap.globals.get`.
#[derive(Default, Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.GlobalGetOptions")]
pub(crate) struct GlobalGetOptions {
    /// Population depth for relationship and upload fields. Unset uses the
    /// configured `[depth] default_depth` (as `crap.collections.find_by_id`
    /// does); `0` = return IDs only. Clamped to the configured
    /// `[depth] max_depth`.
    #[lua(optional)]
    pub(crate) depth: Option<i32>,
    /// Locale code for localized fields. Nil = default locale.
    pub(crate) locale: Option<String>,
    /// Skip access control checks (default: `false`). Set to `true` in
    /// trusted internal code to bypass the global's read access function.
    #[lua(optional)]
    pub(crate) override_access: bool,
    /// Include unpublished (draft) content (default: `false`). When the global
    /// has drafts enabled and has been unpublished, a normal read returns it
    /// empty (no field content); set this to `true` to read the draft instead.
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

/// State threaded into `crap.globals.get` — the snapshot registry, the
/// locale config (cloned once at registration time) and the depth bounds.
pub(crate) struct GlobalsGetState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
    /// Default relationship-population `depth` when unset, from `[depth]
    /// default_depth`.
    pub(crate) default_depth: i32,
    /// Upper bound for relationship-population `depth`, from `[depth] max_depth`.
    pub(crate) max_depth: i32,
}

/// Get a global's current value.
#[lua_fn(path = "crap.globals.get", returns = "crap.Document", auto_tx_read)]
fn globals_get(
    state: &GlobalsGetState,
    lua: &Lua,
    #[lua(doc = "Global slug.")] slug: String,
    #[lua(
        ty = "crap.GlobalGetOptions",
        doc = "Optional options (e.g., `{ locale = \"de\", depth = 1 }`)."
    )]
    opts: Option<GlobalGetOptions>,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();
    let conn = get_tx_conn(lua)?;
    let depth = query::clamp_depth(opts.depth, state.default_depth, state.max_depth);

    let locale_ctx =
        LocaleContext::from_locale_string(opts.locale.as_deref(), &state.locale_config)
            .map_err(lua_err)?;
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

    // The registry resolves the populated targets. No `.cache(...)` and no
    // `.populate_singleflight(...)`: Lua CRUD reads run inside hook
    // transactions (see `crap.collections.find`).
    let ctx = ServiceContext::global(&slug, &def)
        .conn(conn)
        .read_hooks(&hooks)
        .user(user.as_ref())
        .ui_locale(ui_locale.clone())
        .override_access(opts.override_access)
        .registry(Some(state.registry.as_ref()))
        .build();

    // Shared operation body — identical semantics on every surface.
    let args = GetGlobalArgs::builder()
        .locale_ctx(locale_ctx)
        .include_drafts(opts.draft)
        .depth(depth)
        .build();

    let doc = GetGlobal::run(&ctx, args).map_err(lua_err)?;

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
    depth_config: &DepthConfig,
) -> Result<()> {
    register_crap_globals_get(
        lua,
        GlobalsGetState {
            registry,
            locale_config: locale_config.clone(),
            default_depth: depth_config.default_depth,
            max_depth: depth_config.max_depth,
        },
    )?;
    Ok(())
}
