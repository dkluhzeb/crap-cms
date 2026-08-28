//! Registration of `crap.collections.update` Lua function.

use std::sync::Arc;

use crate::hooks::lua_api::utils::lua_err;
use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;

use super::unpublish::{UnpublishCall, unpublish_via_service};

use crate::{
    config::{LocaleConfig, PasswordPolicy},
    core::Registry,
    db::LocaleContext,
    hooks::{
        lifecycle::converters::document_to_lua_table,
        lua_api::crud::{
            get_tx_conn,
            helpers::{
                ExtractedData, check_hook_depth, extract_data, hook_invalidation_transport,
                hook_lua_infra, hook_ui_locale, hook_user, resolve_collection,
            },
        },
    },
    service::{
        LuaWriteHooks, ServiceContext,
        op::{Operation, Update, UpdateArgs},
    },
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Optional options for `crap.collections.update`.
#[derive(Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.UpdateOptions")]
pub(crate) struct UpdateOptions {
    /// Locale code for localized fields. Nil = default locale.
    pub(crate) locale: Option<String>,
    /// Skip access control checks (default: `false`). Set to `true` in
    /// trusted internal code to bypass collection-level and field-level
    /// access for the current user.
    #[lua(optional)]
    pub(crate) override_access: bool,
    /// When `true` and the collection has `versions.drafts`, performs a
    /// version-only save (main table unchanged, only a draft version
    /// snapshot is created).
    #[lua(optional)]
    pub(crate) draft: bool,
    /// Run lifecycle hooks (default: `true`). Set `false` to bypass hooks.
    #[lua(optional)]
    pub(crate) hooks: bool,
    /// When `true`, sets `_status` to `"draft"` (unpublishes). Data is not
    /// modified. Requires `versions` on the collection — errors otherwise.
    #[lua(optional)]
    pub(crate) unpublish: bool,
    /// Emit a live-update event for the updated document (default: `true`).
    /// Set `false` for a quiet write.
    #[lua(optional)]
    pub(crate) events: bool,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            locale: None,
            override_access: false,
            draft: false,
            hooks: true,
            unpublish: false,
            events: true,
        }
    }
}

impl FromLua for UpdateOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// State threaded into `crap.collections.update`.
pub(crate) struct CollectionsUpdateState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
    pub(crate) password_policy: PasswordPolicy,
}

/// Update an existing document.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(path = "crap.collections.update", returns = "crap.Document", auto_tx)]
fn collections_update(
    state: &CollectionsUpdateState,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(doc = "Document ID.")] id: String,
    #[lua(ty = "table<string, any>", doc = "Fields to update (partial).")] data: Table,
    #[lua(
        ty = "crap.UpdateOptions",
        doc = "Optional options (e.g., `{ locale = \"de\" }`)."
    )]
    opts: Option<UpdateOptions>,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();
    let reg = &state.registry;
    let lc = &state.locale_config;

    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let lua_infra = hook_lua_infra(lua);
    let locale_ctx =
        LocaleContext::from_locale_string(opts.locale.as_deref(), lc).map_err(lua_err)?;
    let def = resolve_collection(reg, &collection)?;

    // The unpublish option routes through the same service path as the
    // dedicated `crap.collections.unpublish` — access check, hooks, cache
    // invalidation, mutation event — and errors without versioning.
    if opts.unpublish {
        return unpublish_via_service(
            lua,
            reg,
            &UnpublishCall::builder(&collection, &id)
                .override_access(opts.override_access)
                .hooks(opts.hooks)
                .events(opts.events)
                .build(),
        );
    }

    let ExtractedData { data, password } = extract_data(&data, &def)?;

    let (hooks_enabled, _guard) = check_hook_depth(lua, opts.hooks, &collection, "update");

    let write_hooks = LuaWriteHooks::builder(lua)
        .override_access(opts.override_access)
        .registry(Some(reg.as_ref()))
        .hooks_enabled(hooks_enabled)
        .build();

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .ui_locale(ui_locale.clone())
        .override_access(opts.override_access)
        .emit_events(opts.events)
        .lua_infra(lua_infra.as_ref())
        .invalidation_transport(hook_invalidation_transport(lua))
        .password_policy(Some(&state.password_policy))
        .build();

    // Shared operation body — identical write semantics on every surface.
    let args = UpdateArgs::builder(id.as_str(), data)
        .password(password)
        .locale_ctx(locale_ctx)
        .draft(opts.draft)
        .events(opts.events)
        .build();

    let (doc, _) =
        Update::run(&ctx, args).map_err(|e| RuntimeError(format!("update error: {e:#}")))?;

    document_to_lua_table(lua, &doc)
}

lua_table! {
    name: crap_collections_update,
    path: "crap.collections",
    state: CollectionsUpdateState,
    fns: [collections_update],
}

/// Register `crap.collections.update(collection, id, data, opts?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_update(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
    password_policy: &PasswordPolicy,
) -> Result<()> {
    register_crap_collections_update(
        lua,
        CollectionsUpdateState {
            registry,
            locale_config: locale_config.clone(),
            password_policy: password_policy.clone(),
        },
    )?;
    Ok(())
}
