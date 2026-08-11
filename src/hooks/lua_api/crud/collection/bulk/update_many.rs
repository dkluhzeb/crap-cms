//! Registration of `crap.collections.update_many` Lua function.

use std::collections::HashMap;
use std::sync::Arc;

use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, DocumentFields, Registry},
    db::{FilterClause, FindQuery, LocaleContext, query::filter::normalize_filter_fields},
    hooks::{
        lifecycle::converters::{lua_table_to_hashmap, lua_table_to_json_map},
        lua_api::crud::{
            filter::convert_where_clause,
            get_tx_conn,
            helpers::{
                EnforceAccessParams, check_hook_depth, enforce_access, hook_invalidation_transport,
                hook_lua_infra, hook_ui_locale, hook_user, resolve_collection,
            },
        },
    },
    service::{self, LuaWriteHooks, ServiceContext, UpdateManyOptions, validate_user_filters},
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Query passed to `crap.collections.update_many(collection, query, data, opts?)`.
#[derive(Debug, Default, Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.UpdateManyQuery")]
pub(crate) struct UpdateManyQueryInput {
    #[serde(rename = "where")]
    #[lua(
        rename = "where",
        ty = "table<string, crap.FilterValue | crap.OrCondition[]>",
        optional
    )]
    pub(crate) where_: Option<HashMap<String, serde_json::Value>>,
}

impl FromLua for UpdateManyQueryInput {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

impl UpdateManyQueryInput {
    pub(crate) fn into_find_query(self) -> LuaResult<FindQuery> {
        let filters = match self.where_ {
            Some(w) => convert_where_clause(w)?,
            None => Vec::new(),
        };
        Ok(FindQuery::builder().filters(filters).build())
    }
}

/// Optional options for `crap.collections.update_many`. Standalone from the
/// single-update options so the bulk-only `events` default (`false`) is
/// explicit (and bulk-irrelevant keys like `unpublish` are not accepted).
#[derive(Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.UpdateManyOptions")]
pub(crate) struct UpdateManyOpts {
    /// Locale code for localized fields. Nil = default locale.
    pub(crate) locale: Option<String>,
    /// Skip access control checks (default: `false`).
    #[lua(optional)]
    pub(crate) override_access: bool,
    /// Apply changes to draft versions (default: `false`).
    #[lua(optional)]
    pub(crate) draft: bool,
    /// Run lifecycle hooks (default: `true`). Set `false` to bypass.
    #[lua(optional)]
    pub(crate) hooks: bool,
    /// Emit a live-update event per modified document (default: `false` —
    /// bulk operations are quiet). Set `true` to notify subscribers.
    #[lua(optional)]
    pub(crate) events: bool,
}

impl Default for UpdateManyOpts {
    fn default() -> Self {
        Self {
            locale: None,
            override_access: false,
            draft: false,
            hooks: true,
            events: false,
        }
    }
}

impl FromLua for UpdateManyOpts {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// State threaded into `crap.collections.update_many`.
pub(crate) struct CollectionsUpdateManyState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
    pub(crate) bulk_max_documents: i64,
}

/// Update multiple documents matching a query. All-or-nothing: checks
/// update access for every matched document first; if any fails, returns
/// an error and nothing is modified. Runs the full per-document lifecycle
/// (`before_validate`, field validation, `before_change`, `after_change`)
/// by default; pass `hooks = false` in opts to skip hooks and validation for
/// large batches. Inside hooks, runs within the parent operation's
/// transaction.
#[lua_fn(
    path = "crap.collections.update_many",
    returns = "crap.UpdateManyResult",
    auto_tx
)]
fn collections_update_many(
    state: &CollectionsUpdateManyState,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(ty = "crap.UpdateManyQuery", doc = "Query to match documents.")]
    query: UpdateManyQueryInput,
    #[lua(
        ty = "table<string, any>",
        doc = "Fields to update on all matched documents."
    )]
    data: Table,
    #[lua(ty = "crap.UpdateManyOptions", doc = "Optional options.")] opts: Option<UpdateManyOpts>,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();
    let reg = &state.registry;
    let lc = &state.locale_config;

    let conn = get_tx_conn(lua)?;

    let locale_ctx = LocaleContext::from_locale_string(opts.locale.as_deref(), lc)
        .map_err(|e| RuntimeError(e.to_string()))?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let lua_infra = hook_lua_infra(lua);
    let def = resolve_collection(reg, &collection)?;

    let (hooks_enabled, _guard) = check_hook_depth(lua, opts.hooks, &collection, "update_many");

    let stringified = lua_table_to_hashmap(&data)?;

    if def.is_auth_collection() && stringified.contains_key("password") {
        return Err(RuntimeError(
            "Cannot set password via update_many. Use single update instead.".into(),
        ));
    }

    let mut data_map = service::values_from_strings(stringified);
    let composite_data: DocumentFields = lua_table_to_json_map(&data)?
        .into_iter()
        .filter(|(_, v)| !matches!(v, serde_json::Value::String(_)))
        .collect();
    data_map.extend(composite_data);

    // Built before the access pre-flight so the update access fn sees the
    // incoming patch as `ctx.data` — same contract as the per-document check.
    let filters = build_update_filters(
        lua,
        &def,
        &collection,
        opts.override_access,
        query,
        &data_map,
    )?;

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(opts.override_access)
        .registry(Some(reg.as_ref()))
        .hooks_enabled(hooks_enabled)
        .run_validation(opts.hooks)
        .build();

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .emit_events(opts.events)
        .lua_infra(lua_infra.as_ref())
        .invalidation_transport(hook_invalidation_transport(lua))
        .build();

    let update_opts = UpdateManyOptions {
        locale_ctx: locale_ctx.as_ref(),
        run_hooks: hooks_enabled,
        draft: opts.draft,
        ui_locale: ui_locale.clone(),
        max_documents: state.bulk_max_documents,
    };

    let svc_result = service::update_many(&ctx, &filters, &data_map, lc, &update_opts)
        .map_err(|e| RuntimeError(format!("{e:#}")))?;

    let result = lua.create_table()?;
    result.set("modified", svc_result.modified)?;

    Ok(result)
}

lua_table! {
    name: crap_collections_update_many,
    path: "crap.collections",
    state: CollectionsUpdateManyState,
    fns: [collections_update_many],
}

/// Register `crap.collections.update_many(collection, query, data, opts?)`.
/// Parent `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_update_many(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
    bulk_max_documents: i64,
) -> anyhow::Result<()> {
    register_crap_collections_update_many(
        lua,
        CollectionsUpdateManyState {
            registry,
            locale_config: locale_config.clone(),
            bulk_max_documents,
        },
    )?;
    Ok(())
}

/// Build filters for the bulk update query, enforcing access constraints.
/// `data` is the incoming patch, surfaced to the access fn as `ctx.data`.
fn build_update_filters(
    lua: &Lua,
    def: &CollectionDefinition,
    collection: &str,
    override_access: bool,
    query: UpdateManyQueryInput,
    data: &DocumentFields,
) -> LuaResult<Vec<FilterClause>> {
    let mut find_query = query.into_find_query()?;
    normalize_filter_fields(&mut find_query.filters, &def.fields);
    validate_user_filters(&find_query.filters).map_err(|e| RuntimeError(format!("{e}")))?;

    enforce_access(
        lua,
        &EnforceAccessParams {
            slug: collection,
            override_access,
            access_fn: def.access.update.as_ref(),
            id: None,
            data: Some(data),
            deny_msg: "Update access denied",
            operation: "update",
            injecting_status: false,
        },
        &mut find_query.filters,
    )?;

    Ok(find_query.filters)
}
