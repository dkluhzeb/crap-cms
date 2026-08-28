//! Registration of `crap.collections.find_by_id` Lua function.

use std::sync::Arc;

use crate::hooks::lua_api::utils::lua_err;
use anyhow::Result;
use mlua::{FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;

use crate::{
    config::{DepthConfig, LocaleConfig},
    core::Registry,
    db::{LocaleContext, query},
    hooks::{
        lifecycle::converters::document_to_lua_table,
        lua_api::crud::{
            get_tx_conn,
            helpers::{check_hook_depth, hook_ui_locale, hook_user, resolve_collection},
        },
    },
    service::{
        LuaReadHooks, ServiceContext,
        op::{FindById, FindByIdArgs, Operation},
    },
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Optional options for `crap.collections.find_by_id`.
#[derive(Default, Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.FindByIdOptions")]
pub(crate) struct FindByIdOptions {
    /// Population depth for relationship fields. Unset uses the configured
    /// `[depth] default_depth` (matching the gRPC/MCP surfaces); `0` = return IDs
    /// only. Clamped to the configured `[depth] max_depth`.
    #[lua(optional)]
    pub(crate) depth: Option<i32>,
    /// Locale code for localized fields (e.g., `"en"`, `"de"`, `"all"`).
    /// Nil = default locale.
    pub(crate) locale: Option<String>,
    /// Fields to return. Nil or empty = all fields. `id` is always
    /// included.
    pub(crate) select: Option<Vec<String>>,
    /// When `true` and the collection has `versions.drafts`, returns
    /// the latest draft version snapshot instead of the published
    /// main-table data.
    #[lua(optional)]
    pub(crate) draft: bool,
    /// When `true` and the collection has `soft_delete`, looks up the
    /// document among soft-deleted (trash) rows instead of live ones.
    #[lua(optional)]
    pub(crate) trash: bool,
    /// Skip access control checks (default: `false`). Set to `true` in
    /// trusted internal code to bypass collection-level and field-level
    /// access for the current user.
    #[lua(optional)]
    pub(crate) override_access: bool,
}

impl FromLua for FindByIdOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// State threaded into `crap.collections.find_by_id`.
pub(crate) struct CollectionsFindByIdState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
    /// Default relationship-population `depth` when unset, from `[depth]
    /// default_depth`.
    pub(crate) default_depth: i32,
    /// Upper bound for relationship-population `depth`, from `[depth] max_depth`.
    pub(crate) max_depth: i32,
}

/// Find a single document by ID. Returns `nil` if not found.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.find_by_id",
    returns = "crap.Document?",
    auto_tx
)]
fn collections_find_by_id(
    state: &CollectionsFindByIdState,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(doc = "Document ID.")] id: String,
    #[lua(
        ty = "crap.FindByIdOptions",
        doc = "Optional options (e.g., `{ depth = 1 }`)."
    )]
    opts: Option<FindByIdOptions>,
) -> LuaResult<Value> {
    let opts = opts.unwrap_or_default();
    let reg = &state.registry;
    let lc = &state.locale_config;

    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let depth = query::clamp_depth(opts.depth, state.default_depth, state.max_depth);
    let locale_ctx =
        LocaleContext::from_locale_string(opts.locale.as_deref(), lc).map_err(lua_err)?;
    let def = resolve_collection(reg, &collection)?;

    // Depth guard: a before_read/after_read hook that reads the same
    // collection recurses — cap it like the write paths do.
    let (hooks_enabled, _guard) = check_hook_depth(lua, true, &collection, "find_by_id");

    let hooks = LuaReadHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(opts.override_access)
        .hooks_enabled(hooks_enabled)
        .build();

    // No `.cache(...)` and no `.populate_singleflight(...)`: Lua CRUD reads
    // run inside hook transactions — see the matching note in `find.rs`.
    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .read_hooks(&hooks)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .registry(Some(reg.as_ref()))
        .build();

    // Shared operation body (`FindById::run`): the definition-dependent flag
    // downgrades (draft needs versions, trash needs soft delete) happen there,
    // identically on every surface.
    let args = FindByIdArgs::builder(id)
        .depth(depth)
        .locale_ctx(locale_ctx)
        .select(opts.select)
        .use_draft(opts.draft)
        .include_deleted(opts.trash)
        .build();

    let doc = FindById::run(&ctx, args).map_err(lua_err)?;

    match doc {
        Some(d) => Ok(Value::Table(document_to_lua_table(lua, &d)?)),
        None => Ok(Value::Nil),
    }
}

lua_table! {
    name: crap_collections_find_by_id,
    path: "crap.collections",
    state: CollectionsFindByIdState,
    fns: [collections_find_by_id],
}

/// Register `crap.collections.find_by_id(collection, id, opts?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_find_by_id(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
    depth_config: &DepthConfig,
) -> Result<()> {
    register_crap_collections_find_by_id(
        lua,
        CollectionsFindByIdState {
            registry,
            locale_config: locale_config.clone(),
            default_depth: depth_config.default_depth,
            max_depth: depth_config.max_depth,
        },
    )?;
    Ok(())
}
