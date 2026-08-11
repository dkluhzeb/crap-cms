//! Registration of `crap.collections.create` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;

use crate::{
    config::LocaleConfig,
    core::Registry,
    db::LocaleContext,
    hooks::{
        lifecycle::converters::document_to_lua_table,
        lua_api::crud::{
            get_tx_conn,
            helpers::{
                ExtractedData, check_hook_depth, extract_data, hook_lua_infra, hook_ui_locale,
                hook_user, resolve_collection,
            },
        },
    },
    service::{LuaWriteHooks, ServiceContext, WriteInput, create_document},
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Optional options for `crap.collections.create`.
#[derive(Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.CreateOptions")]
pub(crate) struct CreateOptions {
    /// Locale code for localized fields. Nil = default locale.
    pub(crate) locale: Option<String>,
    /// Skip access control checks (default: `false`). Set to `true` in
    /// trusted internal code to bypass collection-level and field-level
    /// access for the current user.
    #[lua(optional)]
    pub(crate) override_access: bool,
    /// When `true` and the collection has `versions.drafts`, creates the
    /// document with `_status = 'draft'` and skips required-field
    /// validation.
    #[lua(optional)]
    pub(crate) draft: bool,
    /// Run lifecycle hooks (default: `true`). Set `false` to bypass
    /// hooks (e.g., for seeding/migrations).
    #[lua(optional)]
    pub(crate) hooks: bool,
    /// Emit a live-update event for the created document (default: `true`).
    /// Set `false` for a quiet write (e.g., seeding/migrations).
    #[lua(optional)]
    pub(crate) events: bool,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            locale: None,
            override_access: false,
            draft: false,
            hooks: true,
            events: true,
        }
    }
}

impl FromLua for CreateOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// State threaded into `crap.collections.create`.
pub(crate) struct CollectionsCreateState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
}

/// Create a new document.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(path = "crap.collections.create", returns = "crap.Document", auto_tx)]
fn collections_create(
    state: &CollectionsCreateState,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(ty = "table<string, any>", doc = "Field values.")] data: Table,
    #[lua(
        ty = "crap.CreateOptions",
        doc = "Optional options (e.g., `{ locale = \"de\" }`)."
    )]
    opts: Option<CreateOptions>,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();
    let reg = &state.registry;
    let lc = &state.locale_config;

    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let lua_infra = hook_lua_infra(lua);
    let locale_ctx = LocaleContext::from_locale_string(opts.locale.as_deref(), lc)
        .map_err(|e| RuntimeError(e.to_string()))?;
    let def = resolve_collection(reg, &collection)?;

    let ExtractedData { data, password } = extract_data(&data, &def)?;

    let (hooks_enabled, _guard) = check_hook_depth(lua, opts.hooks, &collection, "create");

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(opts.override_access)
        .registry(Some(reg.as_ref()))
        .hooks_enabled(hooks_enabled)
        .build();

    let write_input = WriteInput::builder(data)
        .password(password.as_deref())
        .locale_ctx(locale_ctx.as_ref())
        .locale(opts.locale)
        .draft(opts.draft)
        .ui_locale(ui_locale.clone())
        .build();

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .emit_events(opts.events)
        .lua_infra(lua_infra.as_ref())
        .build();

    let (doc, _) = create_document(&ctx, write_input)
        .map_err(|e| RuntimeError(format!("create error: {e:#}")))?;

    document_to_lua_table(lua, &doc)
}

lua_table! {
    name: crap_collections_create,
    path: "crap.collections",
    state: CollectionsCreateState,
    fns: [collections_create],
}

/// Register `crap.collections.create(collection, data, opts?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_create(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    register_crap_collections_create(
        lua,
        CollectionsCreateState {
            registry,
            locale_config: locale_config.clone(),
        },
    )?;
    Ok(())
}
