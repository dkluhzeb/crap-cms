//! Unpublish operation — reverts a published document to draft status.

use std::sync::Arc;

use mlua::{
    Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value as LuaValue,
};
use serde::Deserialize;

use anyhow::Result;

use crate::{
    core::Registry,
    hooks::{
        lifecycle::converters::document_to_lua_table,
        lua_api::crud::{
            get_tx_conn,
            helpers::{
                check_hook_depth, hook_invalidation_transport, hook_locale_config, hook_lua_infra,
                hook_ui_locale, hook_user, resolve_collection,
            },
        },
    },
    service::{LuaWriteHooks, ServiceContext, unpublish_document},
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Optional options for `crap.collections.unpublish`.
#[derive(Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.UnpublishOptions")]
pub(crate) struct UnpublishOptions {
    /// Skip access control checks (default: `false`).
    #[lua(optional)]
    pub(crate) override_access: bool,
    /// Run lifecycle hooks (default: `true`).
    #[lua(optional)]
    pub(crate) hooks: bool,
}

impl Default for UnpublishOptions {
    fn default() -> Self {
        Self {
            override_access: false,
            hooks: true,
        }
    }
}

impl FromLua for UnpublishOptions {
    fn from_lua(value: LuaValue, lua: &Lua) -> LuaResult<Self> {
        match value {
            LuaValue::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// Parameters for the shared unpublish routing.
pub(super) struct UnpublishCall<'a> {
    collection: &'a str,
    id: &'a str,
    override_access: bool,
    hooks: bool,
    events: bool,
}

impl<'a> UnpublishCall<'a> {
    /// Create a builder with the required fields.
    pub(super) fn builder(collection: &'a str, id: &'a str) -> UnpublishCallBuilder<'a> {
        UnpublishCallBuilder::new(collection, id)
    }
}

/// Builder for [`UnpublishCall`].
pub(super) struct UnpublishCallBuilder<'a> {
    collection: &'a str,
    id: &'a str,
    override_access: bool,
    hooks: bool,
    events: bool,
}

impl<'a> UnpublishCallBuilder<'a> {
    fn new(collection: &'a str, id: &'a str) -> Self {
        Self {
            collection,
            id,
            override_access: false,
            hooks: true,
            events: true,
        }
    }

    pub(super) fn override_access(mut self, v: bool) -> Self {
        self.override_access = v;
        self
    }

    pub(super) fn hooks(mut self, v: bool) -> Self {
        self.hooks = v;
        self
    }

    pub(super) fn events(mut self, v: bool) -> Self {
        self.events = v;
        self
    }

    pub(super) fn build(self) -> UnpublishCall<'a> {
        UnpublishCall {
            collection: self.collection,
            id: self.id,
            override_access: self.override_access,
            hooks: self.hooks,
            events: self.events,
        }
    }
}

/// Route an unpublish through the full service path — access check (incl.
/// row-level constraints), hook depth guard, lifecycle hooks, cache
/// invalidation, and mutation event. Shared by `crap.collections.unpublish`
/// and the `unpublish = true` option on `crap.collections.update`, so both
/// surfaces behave identically.
pub(super) fn unpublish_via_service(
    lua: &Lua,
    registry: &Arc<Registry>,
    call: &UnpublishCall<'_>,
) -> LuaResult<Table> {
    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let lua_infra = hook_lua_infra(lua);
    let def = resolve_collection(registry, call.collection)?;

    // Capability gate (versioning required) is enforced at the shared service
    // chokepoint `service::unpublish_document`, so every surface agrees.

    let (hooks_enabled, _guard) = check_hook_depth(lua, call.hooks, call.collection, "update");

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(call.override_access)
        .registry(Some(registry.as_ref()))
        .hooks_enabled(hooks_enabled)
        .build();

    let locale_config = hook_locale_config(lua);

    let ctx = ServiceContext::collection(call.collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(call.override_access)
        .emit_events(call.events)
        .locale_config(locale_config.as_ref())
        .lua_infra(lua_infra.as_ref())
        .invalidation_transport(hook_invalidation_transport(lua))
        .build();

    let doc = unpublish_document(&ctx, call.id)
        .map_err(|e| RuntimeError(format!("unpublish error: {e:#}")))?;

    document_to_lua_table(lua, &doc)
}

/// Unpublish a document — sets `_status` to `"draft"` without modifying
/// the underlying field data. Only available on collections with
/// `versions` enabled.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.unpublish",
    returns = "crap.Document",
    auto_tx
)]
fn collections_unpublish(
    state: &Arc<Registry>,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(doc = "Document ID.")] id: String,
    #[lua(ty = "crap.UnpublishOptions", doc = "Optional options.")] opts: Option<UnpublishOptions>,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();

    unpublish_via_service(
        lua,
        state,
        &UnpublishCall::builder(&collection, &id)
            .override_access(opts.override_access)
            .hooks(opts.hooks)
            .build(),
    )
}

lua_table! {
    name: crap_collections_unpublish,
    path: "crap.collections",
    state: Arc<Registry>,
    fns: [collections_unpublish],
}

/// Register `crap.collections.unpublish(collection, id, opts?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_unpublish(lua: &Lua, _table: &Table, registry: Arc<Registry>) -> Result<()> {
    register_crap_collections_unpublish(lua, registry)?;
    Ok(())
}
