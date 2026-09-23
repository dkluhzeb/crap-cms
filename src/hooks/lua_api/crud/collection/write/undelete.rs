//! Registration of `crap.collections.undelete` Lua function.

use std::sync::Arc;

use crate::hooks::lua_api::utils::lua_err;
use anyhow::Result;
use mlua::{FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;

use crate::{
    core::Registry,
    hooks::lua_api::crud::{
        get_tx_conn,
        helpers::{
            check_hook_depth, hook_invalidation_transport, hook_locale_config, hook_lua_infra,
            hook_ui_locale, hook_user, resolve_collection,
        },
    },
    service::{
        LuaWriteHooks, ServiceContext,
        op::{Operation, Undelete, UndeleteArgs},
    },
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Optional options for `crap.collections.undelete`.
#[derive(Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.UndeleteOptions")]
pub(crate) struct UndeleteOptions {
    /// Skip access control checks (default: `false`).
    #[lua(optional)]
    pub(crate) override_access: bool,
    /// Run lifecycle hooks (default: `true`), as on `unpublish`.
    #[lua(optional)]
    pub(crate) hooks: bool,
    /// Emit a live-update event for the restored document (default: `true`).
    /// Set `false` for a quiet restore. Parity with the gRPC/MCP undelete.
    #[lua(optional)]
    pub(crate) events: bool,
}

impl Default for UndeleteOptions {
    fn default() -> Self {
        Self {
            override_access: false,
            hooks: true,
            events: true,
        }
    }
}

impl FromLua for UndeleteOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// Restore a soft-deleted document. Only available on collections with
/// `soft_delete` enabled.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.undelete",
    returns_doc = "True when the document was successfully restored.",
    auto_tx
)]
fn collections_undelete(
    state: &Arc<Registry>,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(doc = "Document ID.")] id: String,
    #[lua(ty = "crap.UndeleteOptions", doc = "Optional options.")] opts: Option<UndeleteOptions>,
) -> LuaResult<bool> {
    let opts = opts.unwrap_or_default();
    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let lua_infra = hook_lua_infra(lua);
    let def = resolve_collection(state, &collection)?;

    // Capability gate (soft-delete required) is enforced at the shared service
    // chokepoint `service::undelete_document`, so every surface agrees.

    // Undelete runs the lifecycle hooks like every other state write, so the
    // nesting guard applies to it too. Validation stays off: an undelete
    // carries no field edits to validate.
    let (hooks_enabled, _guard) = check_hook_depth(lua, opts.hooks, &collection, "undelete");

    let wh = LuaWriteHooks::builder(lua, state.as_ref())
        .override_access(opts.override_access)
        .hooks_enabled(hooks_enabled)
        .run_validation(false)
        .build();

    // The locale config is what lets the reads around the restore address a
    // localized collection's per-locale columns (`title__en`); without it the
    // SELECT names a bare `title` that does not exist there.
    let locale_config = hook_locale_config(lua);

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .write_hooks(&wh)
        .user(user.as_ref())
        .ui_locale(ui_locale.clone())
        .override_access(opts.override_access)
        .emit_events(opts.events)
        .locale_config(locale_config.as_ref())
        .lua_infra(lua_infra.as_ref())
        .invalidation_transport(hook_invalidation_transport(lua))
        .build();

    // Shared operation body — identical semantics on every surface.
    Undelete::run(&ctx, UndeleteArgs::new(id.as_str()).events(opts.events)).map_err(lua_err)?;

    Ok(true)
}

lua_table! {
    name: crap_collections_undelete,
    path: "crap.collections",
    state: Arc<Registry>,
    fns: [collections_undelete],
}

/// Register `crap.collections.undelete(collection, id, opts?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_undelete(lua: &Lua, _table: &Table, registry: Arc<Registry>) -> Result<()> {
    register_crap_collections_undelete(lua, registry)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (wire-parity): `crap.collections.undelete` rejected an
    /// `events` option (`deny_unknown_fields`) while gRPC and MCP both offer
    /// one — a Lua caller could not do a quiet restore. The option now parses
    /// and defaults to `true` like every single-document write.
    #[test]
    fn undelete_options_accept_events_flag() {
        let lua = Lua::new();

        let opts: UndeleteOptions = lua
            .from_value(
                lua.to_value(&serde_json::json!({ "events": false }))
                    .unwrap(),
            )
            .unwrap();
        assert!(!opts.events);
        assert!(!opts.override_access);

        assert!(UndeleteOptions::default().events, "quiet must be opt-in");
    }

    /// `hooks` defaults on and can be switched off, exactly like unpublish.
    #[test]
    fn hooks_option_defaults_on_and_can_be_disabled() {
        let lua = mlua::Lua::new();
        assert!(UndeleteOptions::default().hooks);

        let table: mlua::Table = lua.load("return { hooks = false }").eval().unwrap();
        let opts: UndeleteOptions = lua.from_value(mlua::Value::Table(table)).unwrap();
        assert!(!opts.hooks);
    }
}
