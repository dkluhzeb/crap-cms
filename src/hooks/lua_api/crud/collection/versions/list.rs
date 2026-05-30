//! Registration of `crap.collections.list_versions` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::{Deserialize, Serialize};

use crate::{
    core::Registry,
    db::query::PaginationResult,
    hooks::lua_api::crud::{
        get_tx_conn,
        helpers::{hook_user, resolve_collection},
    },
    service::{ListVersionsInput, LuaReadHooks, ServiceContext, list_versions},
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Optional options for `crap.collections.list_versions`.
#[derive(Default, Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.ListVersionsOptions")]
pub(crate) struct ListVersionsOptions {
    /// Max number of versions to return.
    pub(crate) limit: Option<i64>,
    /// Offset for pagination.
    pub(crate) offset: Option<i64>,
    /// Skip access control checks (default: `false`). Set to `true` in
    /// trusted internal code (jobs, migrations) to bypass collection-level
    /// read-access checks.
    #[serde(rename = "overrideAccess")]
    #[lua(rename = "overrideAccess", optional)]
    pub(crate) override_access: bool,
}

impl FromLua for ListVersionsOptions {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// One row of `crap.collections.list_versions(...)`. Projection of
/// `core::document::VersionSnapshot` — drops the full `snapshot` payload
/// and the `parent` ID since the Lua user already supplied the parent.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.VersionSummary")]
pub(crate) struct VersionSummary {
    /// Version row ID (not the parent document ID).
    id: String,
    /// Monotonically-increasing version number, newest first.
    version: i64,
    /// Version status (e.g. `"published"`, `"draft"`).
    status: String,
    /// `true` for the newest version, `false` otherwise.
    latest: bool,
    /// ISO 8601 timestamp the snapshot was taken (if recorded).
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
}

/// Result of `crap.collections.list_versions(...)`.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.ListVersionsResult")]
pub(crate) struct ListVersionsResult {
    /// Versions, newest first.
    docs: Vec<VersionSummary>,
    /// Pagination metadata.
    #[lua(ty = "crap.PaginationInfo")]
    pagination: PaginationResult,
}

/// List version snapshots for a document, newest first. Returns a table
/// of [`crap.VersionSummary`](lua://crap.VersionSummary) rows plus
/// pagination metadata. Only available on collections with `versions`
/// enabled.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.list_versions",
    returns = "crap.ListVersionsResult",
    auto_tx
)]
fn collections_list_versions(
    state: &Arc<Registry>,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(doc = "Parent document ID.")] id: String,
    #[lua(ty = "crap.ListVersionsOptions", doc = "Optional options.")] opts: Option<
        ListVersionsOptions,
    >,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();
    let conn = get_tx_conn(lua)?;

    let def = resolve_collection(state, &collection)?;

    let user = hook_user(lua);
    let hooks = LuaReadHooks::builder(lua)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .build();

    let ctx = ServiceContext::collection(&collection, &def)
        .conn(conn)
        .read_hooks(&hooks)
        .user(user.as_ref())
        .override_access(opts.override_access)
        .build();

    let input = ListVersionsInput::builder(&id)
        .limit(opts.limit)
        .offset(opts.offset)
        .build();

    let paginated = list_versions(&ctx, &input).map_err(|e| RuntimeError(format!("{e}")))?;

    let result = ListVersionsResult {
        docs: paginated
            .docs
            .into_iter()
            .map(|v| VersionSummary {
                id: v.id,
                version: v.version,
                status: v.status,
                latest: v.latest,
                created_at: v.created_at,
            })
            .collect(),
        pagination: paginated.pagination,
    };

    let value = lua.to_value(&result)?;
    let Value::Table(tbl) = value else {
        return Err(RuntimeError(
            "ListVersionsResult did not serialize to a table".into(),
        ));
    };
    Ok(tbl)
}

lua_table! {
    name: crap_collections_list_versions,
    path: "crap.collections",
    state: Arc<Registry>,
    fns: [collections_list_versions],
}

/// Register `crap.collections.list_versions(collection, id, opts?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_list_versions(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
) -> Result<()> {
    register_crap_collections_list_versions(lua, registry)?;
    Ok(())
}
