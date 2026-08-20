//! Registration of `crap.collections.create_many` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Deserialize;

use crate::{
    config::PasswordPolicy,
    core::{CollectionDefinition, Registry},
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
    service::{self, CreateManyItem, CreateManyOptions, LuaWriteHooks, ServiceContext},
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Optional options for `crap.collections.create_many`. Standalone from the
/// single-create options so the bulk-only `events` default (`false`) is
/// explicit.
#[derive(Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.CreateManyOptions")]
pub(crate) struct CreateManyOpts {
    /// Skip access control checks (default: `false`).
    #[lua(optional)]
    pub(crate) override_access: bool,
    /// Create documents as drafts (default: `false`).
    #[lua(optional)]
    pub(crate) draft: bool,
    /// Run lifecycle hooks (default: `true`). Set `false` to bypass.
    #[lua(optional)]
    pub(crate) hooks: bool,
    /// Emit a live-update event per created document (default: `false` —
    /// bulk operations are quiet). Set `true` to notify subscribers.
    #[lua(optional)]
    pub(crate) events: bool,
}

impl Default for CreateManyOpts {
    fn default() -> Self {
        Self {
            override_access: false,
            draft: false,
            hooks: true,
            events: false,
        }
    }
}

impl FromLua for CreateManyOpts {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

/// Bulk create multiple documents from an array of data tables.
/// All-or-nothing: a single transaction wraps every insert.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(
    path = "crap.collections.create_many",
    returns = "crap.CreateManyResult",
    auto_tx
)]
fn collections_create_many(
    state: &CollectionsCreateManyState,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(
        ty = "table<string, any>[]",
        doc = "Array of field-value tables — one per document."
    )]
    items: Table,
    #[lua(
        ty = "crap.CreateManyOptions",
        doc = "Optional options applied to every document."
    )]
    opts: Option<CreateManyOpts>,
) -> LuaResult<Table> {
    let opts = opts.unwrap_or_default();
    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let lua_infra = hook_lua_infra(lua);
    let def = resolve_collection(&state.registry, &collection)?;

    let (hooks_enabled, _guard) = check_hook_depth(lua, opts.hooks, &collection, "create_many");

    let mut parsed_items = Vec::new();
    for i in 1..=items.raw_len() {
        let item_val: Value = items.raw_get(i)?;

        let Value::Table(item_table) = item_val else {
            return Err(RuntimeError(format!(
                "create_many: item at index {i} is not a table"
            )));
        };

        parsed_items.push(parse_item(&item_table, &def)?);
    }

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(opts.override_access)
        .registry(Some(state.registry.as_ref()))
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
        .password_policy(Some(&state.password_policy))
        .build();

    let create_opts = CreateManyOptions {
        run_hooks: hooks_enabled,
        draft: opts.draft,
        max_documents: state.bulk_max_documents,
    };

    let svc_result = service::create_many(&ctx, &parsed_items, &create_opts)
        .map_err(|e| RuntimeError(format!("{e:#}")))?;

    let result = lua.create_table()?;
    result.set("created", svc_result.created)?;

    let docs_table = lua.create_table()?;
    for (i, doc) in svc_result.documents.iter().enumerate() {
        let entry = document_to_lua_table(lua, doc)?;
        docs_table.raw_set(i + 1, entry)?;
    }
    result.set("documents", docs_table)?;

    Ok(result)
}

/// State threaded into `crap.collections.create_many` — the loaded registry
/// plus the `server.bulk_max_documents` cap enforced by the service layer.
pub(crate) struct CollectionsCreateManyState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) bulk_max_documents: i64,
    pub(crate) password_policy: PasswordPolicy,
}

lua_table! {
    name: crap_collections_create_many,
    path: "crap.collections",
    state: CollectionsCreateManyState,
    fns: [collections_create_many],
}

/// Register `crap.collections.create_many(collection, items, opts?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_create_many(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    bulk_max_documents: i64,
    password_policy: &PasswordPolicy,
) -> Result<()> {
    register_crap_collections_create_many(
        lua,
        CollectionsCreateManyState {
            registry,
            bulk_max_documents,
            password_policy: password_policy.clone(),
        },
    )?;
    Ok(())
}

/// Parse a single Lua table item into a `CreateManyItem`. Delegates to
/// `extract_data` so bulk items get exactly the single-`create` semantics —
/// in particular, `password` is only separated out for auth collections (a
/// non-auth collection may have a legitimate field named `password`).
fn parse_item(item_table: &Table, def: &CollectionDefinition) -> LuaResult<CreateManyItem> {
    let ExtractedData { data, password } = extract_data(item_table, def)?;

    Ok(CreateManyItem { data, password })
}
