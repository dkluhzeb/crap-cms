//! Unpublish operation — reverts a published document to draft status.

use std::sync::Arc;

use mlua::{
    Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value as LuaValue,
};
use serde::Deserialize;

use anyhow::Result;

use crate::{
    core::{Builder, Registry},
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
    service::{
        LuaWriteHooks, ServiceContext,
        op::{Operation, Unpublish, UnpublishArgs},
    },
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
    /// Emit a live-update event for this change (default: `true`). Parity
    /// with `crap.collections.update{ unpublish = true, events = ... }`.
    #[lua(optional)]
    pub(crate) events: bool,
    /// The document revision this write is based on — the `_revision` of the
    /// read it edits. Set, the write fails with a revision conflict when
    /// anyone has written the document since; nil writes unconditionally.
    pub(crate) expected_revision: Option<i64>,
}

impl Default for UnpublishOptions {
    fn default() -> Self {
        Self {
            override_access: false,
            hooks: true,
            events: true,
            expected_revision: None,
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
#[derive(Builder)]
pub(super) struct UnpublishCall<'a> {
    #[builder(required)]
    collection: &'a str,
    #[builder(required)]
    id: &'a str,
    override_access: bool,
    #[builder(default = true)]
    hooks: bool,
    #[builder(default = true)]
    events: bool,
    expected_revision: Option<i64>,
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

    let write_hooks = LuaWriteHooks::builder(lua, registry.as_ref())
        .override_access(call.override_access)
        .hooks_enabled(hooks_enabled)
        .build();

    let locale_config = hook_locale_config(lua);

    let ctx = ServiceContext::collection(call.collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .ui_locale(ui_locale.clone())
        .override_access(call.override_access)
        .emit_events(call.events)
        .locale_config(locale_config.as_ref())
        .lua_infra(lua_infra.as_ref())
        .invalidation_transport(hook_invalidation_transport(lua))
        .build();

    // Shared operation body — identical semantics on every surface.
    let args = UnpublishArgs::builder(call.id)
        .events(call.events)
        .expected_revision(call.expected_revision)
        .build();

    let doc =
        Unpublish::run(&ctx, args).map_err(|e| RuntimeError(format!("unpublish error: {e:#}")))?;

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
            .events(opts.events)
            .expected_revision(opts.expected_revision)
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
