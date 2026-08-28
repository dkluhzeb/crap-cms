//! Registration of `crap.collections.delete_many` Lua function.

use std::collections::HashMap;
use std::sync::Arc;

use crate::hooks::lua_api::utils::lua_err;
use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::{Deserialize, Serialize};

use crate::{
    config::LocaleConfig,
    core::{Registry, upload},
    db::{FilterClause, FindQuery, LocaleContext},
    hooks::{
        lifecycle::LuaVmInfra,
        lua_api::crud::{
            filter::convert_where_clause,
            get_tx_conn,
            helpers::{
                check_hook_depth, hook_invalidation_transport, hook_lua_infra, hook_ui_locale,
                hook_user, resolve_collection,
            },
        },
    },
    service::{
        LuaWriteHooks, ServiceContext,
        op::{DeleteMany, DeleteManyArgs, Operation},
    },
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Query passed to `crap.collections.delete_many(collection, query, opts?)`.
#[derive(Debug, Default, Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.DeleteManyQuery")]
pub(crate) struct DeleteManyQueryInput {
    #[serde(rename = "where")]
    #[lua(
        rename = "where",
        ty = "table<string, crap.FilterValue | crap.OrCondition[]>",
        optional
    )]
    pub(crate) where_: Option<HashMap<String, serde_json::Value>>,
}

impl FromLua for DeleteManyQueryInput {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

impl DeleteManyQueryInput {
    pub(crate) fn into_find_query(self) -> LuaResult<FindQuery> {
        let filters = match self.where_ {
            Some(w) => convert_where_clause(w)?,
            None => Vec::new(),
        };
        Ok(FindQuery::builder().filters(filters).build())
    }
}

/// Result of `crap.collections.delete_many(...)`. `deleted` is the sum
/// of `hard_deleted + soft_deleted` from
/// `service::collections::DeleteManyResult` — the Lua user doesn't care
/// about that distinction.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.DeleteManyResult")]
pub(crate) struct DeleteManyResult {
    /// Number of documents deleted (hard + soft).
    pub(crate) deleted: i64,
    /// Number of documents skipped because they had incoming references.
    pub(crate) skipped: i64,
}

/// Optional options for `crap.collections.delete_many`. Standalone from the
/// single-delete options so the bulk-only `events` default (`false`) is
/// explicit.
#[derive(Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.DeleteManyOptions")]
pub(crate) struct DeleteManyOpts {
    /// Locale code. Validated but not used for matching (`delete_many` spans
    /// locales). Nil = default locale.
    pub(crate) locale: Option<String>,
    /// Skip access control checks (default: `false`).
    #[lua(optional)]
    pub(crate) override_access: bool,
    /// Run lifecycle hooks (default: `true`). Set `false` to bypass.
    #[lua(optional)]
    pub(crate) hooks: bool,
    /// Bypass `soft_delete` and remove rows permanently (default: `false`).
    #[lua(optional)]
    pub(crate) force_hard_delete: bool,
    /// Target already-trashed documents and permanently remove them (empty
    /// the trash). Implies a hard delete gated by `access.delete`; matches
    /// only rows with `_deleted_at` set (default: `false`).
    #[lua(optional)]
    pub(crate) trash: bool,
    /// Emit a live-update event per deleted document (default: `false` —
    /// bulk operations are quiet). Set `true` to notify subscribers.
    #[lua(optional)]
    pub(crate) events: bool,
}

impl Default for DeleteManyOpts {
    fn default() -> Self {
        Self {
            locale: None,
            override_access: false,
            hooks: true,
            force_hard_delete: false,
            trash: false,
            events: false,
        }
    }
}

impl FromLua for DeleteManyOpts {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// State threaded into `crap.collections.delete_many`.
pub(crate) struct CollectionsDeleteManyState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
    pub(crate) bulk_max_documents: i64,
}

/// Delete multiple documents matching a query. All-or-nothing: checks
/// delete access for every matched document first; if any fails, returns
/// an error and nothing is modified. Fires per-document delete hooks
/// (`before_delete`, `after_delete`) by default; pass `hooks = false` in opts
/// to skip them for large batches. Referenced documents are skipped
/// (counted in `skipped`), not errored. Inside hooks, runs within the
/// parent operation's transaction.
#[lua_fn(
    path = "crap.collections.delete_many",
    returns = "crap.DeleteManyResult",
    auto_tx
)]
fn collections_delete_many(
    state: &CollectionsDeleteManyState,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(ty = "crap.DeleteManyQuery", doc = "Query to match documents.")]
    query: DeleteManyQueryInput,
    #[lua(ty = "crap.DeleteManyOptions", doc = "Optional options.")] opts: Option<DeleteManyOpts>,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();
    let reg = &state.registry;
    let lc = &state.locale_config;

    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let lua_infra = hook_lua_infra(lua);
    let def = resolve_collection(reg, &collection)?;

    // Validate the requested locale up front. delete_many matches documents
    // across locales, so the locale isn't used for filtering — but an invalid
    // code should still surface as an error rather than be ignored.
    LocaleContext::from_locale_string(opts.locale.as_deref(), lc).map_err(lua_err)?;

    let filters = build_delete_filters(query)?;

    // The `_deleted_at EXISTS` trash restriction itself lives in the shared
    // operation body; this codec only gates the capability — a collection
    // without soft delete has no trash (or `_deleted_at` column) to purge.
    if opts.trash && !def.soft_delete {
        return Err(RuntimeError(format!(
            "Collection '{collection}' does not have soft delete enabled; there is no trash to purge"
        )));
    }

    let (hooks_enabled, _guard) = check_hook_depth(lua, opts.hooks, &collection, "delete_many");

    let write_hooks = LuaWriteHooks::builder(lua)
        .override_access(opts.override_access)
        .registry(Some(reg.as_ref()))
        .hooks_enabled(hooks_enabled)
        .build();

    // Clear `soft_delete` on the service def for a hard delete OR a trash
    // purge, so the per-row delete hard-removes and its read doesn't append
    // `_deleted_at IS NULL` (which would hide the already-trashed rows).
    // Mirrors the admin empty-trash path.
    let mut service_def = def.clone();
    if opts.force_hard_delete || opts.trash {
        service_def.make_hard_delete();
    }

    let invalidation_transport = hook_invalidation_transport(lua);

    let ctx = ServiceContext::collection(&collection, &service_def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .ui_locale(ui_locale.clone())
        .override_access(opts.override_access)
        .invalidation_transport(invalidation_transport)
        .emit_events(opts.events)
        .lua_infra(lua_infra.as_ref())
        .locale_config(Some(lc))
        .build();

    // Shared operation body. Conn mode: the definition was already adjusted
    // above (Lua also purges trash via the same hard-delete rule) and file
    // cleanup stays with this surface — see the pool-mode note in the op.
    let op_args = DeleteManyArgs::builder(filters)
        .run_hooks(hooks_enabled)
        .max_documents(state.bulk_max_documents)
        .trash(opts.trash)
        .events(opts.events)
        .build();

    let svc_result = DeleteMany::run(&ctx, op_args).map_err(lua_err)?;

    if !service_def.soft_delete
        && let Some(storage) = lua
            .app_data_ref::<LuaVmInfra>()
            .and_then(|i| i.storage.clone())
    {
        for fields in &svc_result.upload_fields_to_clean {
            upload::delete_upload_files(&*storage, fields);
        }
    }

    let result = DeleteManyResult {
        deleted: svc_result.hard_deleted + svc_result.soft_deleted,
        skipped: svc_result.skipped,
    };

    let Value::Table(tbl) = lua.to_value(&result)? else {
        return Err(RuntimeError(
            "DeleteManyResult did not serialize to a table".into(),
        ));
    };
    Ok(tbl)
}

lua_table! {
    name: crap_collections_delete_many,
    path: "crap.collections",
    state: CollectionsDeleteManyState,
    fns: [collections_delete_many],
}

/// Register `crap.collections.delete_many(collection, query, opts?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_delete_many(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
    bulk_max_documents: i64,
) -> Result<()> {
    register_crap_collections_delete_many(
        lua,
        CollectionsDeleteManyState {
            registry,
            locale_config: locale_config.clone(),
            bulk_max_documents,
        },
    )?;
    Ok(())
}

/// Decode the bulk delete query into canonical filters. Pure decode — filter
/// hygiene (system columns, dot paths) lives in the shared `DeleteMany` body,
/// and access gating + constraint scoping at the service chokepoint
/// (`service::collections::bulk_access`) — the trash-vs-delete gate derives
/// from the (possibly adjusted) definition.
fn build_delete_filters(query: DeleteManyQueryInput) -> LuaResult<Vec<FilterClause>> {
    Ok(query.into_find_query()?.filters)
}
