//! Registration of `crap.collections.delete_many` Lua function.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::{Deserialize, Serialize};

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, HookRef, Registry, upload},
    db::{FilterClause, FindQuery, LocaleContext, query::filter::normalize_filter_fields},
    hooks::{
        lifecycle::LuaStorage,
        lua_api::crud::{
            filter::convert_where_clause,
            get_tx_conn,
            helpers::{
                EnforceAccessParams, check_hook_depth, enforce_access, hook_invalidation_transport,
                hook_lua_infra, hook_ui_locale, hook_user, resolve_collection,
            },
        },
    },
    service::{self, DeleteManyOptions, LuaWriteHooks, ServiceContext, validate_user_filters},
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
    #[serde(rename = "overrideAccess")]
    #[lua(rename = "overrideAccess", optional)]
    pub(crate) override_access: bool,
    /// Run lifecycle hooks (default: `true`). Set `false` to bypass.
    #[lua(optional)]
    pub(crate) hooks: bool,
    /// Bypass `soft_delete` and remove rows permanently (default: `false`).
    #[serde(rename = "forceHardDelete")]
    #[lua(rename = "forceHardDelete", optional)]
    pub(crate) force_hard_delete: bool,
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
    let soft_delete = def.soft_delete && !opts.force_hard_delete;

    // Validate the requested locale up front. delete_many matches documents
    // across locales, so the locale isn't used for filtering — but an invalid
    // code should still surface as an error rather than be ignored.
    LocaleContext::from_locale_string(opts.locale.as_deref(), lc)
        .map_err(|e| RuntimeError(e.to_string()))?;

    let filters = build_delete_filters(
        lua,
        &def,
        &collection,
        soft_delete,
        opts.override_access,
        query,
    )?;

    let (hooks_enabled, _guard) = check_hook_depth(lua, opts.hooks, &collection, "delete_many");

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(opts.override_access)
        .registry(Some(reg.as_ref()))
        .hooks_enabled(hooks_enabled)
        .build();

    let mut service_def = def.clone();
    if opts.force_hard_delete {
        service_def.soft_delete = false;
    }

    let invalidation_transport = hook_invalidation_transport(lua);

    let ctx = ServiceContext::collection(&collection, &service_def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .invalidation_transport(invalidation_transport)
        .emit_events(opts.events)
        .lua_infra(lua_infra.as_ref())
        .build();

    let delete_opts = DeleteManyOptions {
        run_hooks: hooks_enabled,
        max_documents: state.bulk_max_documents,
        ..Default::default()
    };

    let svc_result = service::delete_many(&ctx, &filters, lc, &delete_opts)
        .map_err(|e| RuntimeError(format!("{e}")))?;

    if !service_def.soft_delete
        && let Some(lua_storage) = lua.app_data_ref::<LuaStorage>()
    {
        for fields in &svc_result.upload_fields_to_clean {
            upload::delete_upload_files(&*lua_storage.0, fields);
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

/// Resolve the access function for delete operations.
fn resolve_delete_access(def: &CollectionDefinition, soft_delete: bool) -> Option<&HookRef> {
    if soft_delete {
        def.access.resolve_trash()
    } else {
        def.access.delete.as_ref()
    }
}

/// Build filters for the bulk delete query, enforcing access constraints.
fn build_delete_filters(
    lua: &Lua,
    def: &CollectionDefinition,
    collection: &str,
    soft_delete: bool,
    override_access: bool,
    query: DeleteManyQueryInput,
) -> LuaResult<Vec<FilterClause>> {
    let mut find_query = query.into_find_query()?;
    normalize_filter_fields(&mut find_query.filters, &def.fields);
    validate_user_filters(&find_query.filters).map_err(|e| RuntimeError(format!("{e}")))?;

    let access_ref = resolve_delete_access(def, soft_delete);

    enforce_access(
        lua,
        &EnforceAccessParams {
            slug: collection,
            override_access,
            access_fn: access_ref,
            id: None,
            data: None,
            // Soft delete → "trash" (trash access fn); hard delete → "delete".
            deny_msg: if soft_delete {
                "Trash access denied"
            } else {
                "Delete access denied"
            },
            operation: if soft_delete { "trash" } else { "delete" },
            injecting_status: false,
        },
        &mut find_query.filters,
    )?;

    Ok(find_query.filters)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_trash_access_when_soft_deleting_else_delete() {
        let mut def = CollectionDefinition::new("posts");
        def.access.delete = Some("can_delete".into());
        def.access.trash = Some("can_trash".into());

        assert_eq!(
            resolve_delete_access(&def, false),
            Some(&HookRef::new("can_delete"))
        );
        assert_eq!(
            resolve_delete_access(&def, true),
            Some(&HookRef::new("can_trash"))
        );
    }

    #[test]
    fn none_when_access_unset() {
        let def = CollectionDefinition::new("posts");
        assert_eq!(resolve_delete_access(&def, false), None);
        assert_eq!(resolve_delete_access(&def, true), None);
    }
}
