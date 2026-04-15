//! Registration of `crap.collections.update_many` Lua function.

use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    db::{
        FindQuery, LocaleContext,
        query::{self, filter::normalize_filter_fields},
    },
    hooks::lifecycle::{
        converters::{lua_table_to_find_query, lua_table_to_hashmap, lua_table_to_json_map},
        crud::{get_tx_conn, helpers::*},
    },
    service::{self, LuaWriteHooks, WriteInput, validate_user_filters},
};

/// Update multiple documents matching a query with the given data.
///
/// Runs the full per-document lifecycle for each matched document:
/// field hooks, validation, before/after change hooks, DB write,
/// ref count updates, FTS sync, and version snapshots.
fn update_many_documents(
    lua: &Lua,
    reg: &SharedRegistry,
    lc: &LocaleConfig,
    collection: &str,
    query_table: &Table,
    data_table: &Table,
    opts: &Option<Table>,
) -> mlua::Result<Table> {
    // SAFETY: pointer valid for hook call duration — see TxContext pattern
    let conn_ptr = get_tx_conn(lua)?;
    let conn = unsafe { &*conn_ptr };

    let locale_str = get_opt_string(opts, "locale")?;
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc)
        .map_err(|e| RuntimeError(e.to_string()))?;
    let override_access = get_opt_bool(opts, "overrideAccess", false)?;
    let run_hooks = get_opt_bool(opts, "hooks", true)?;
    let draft = get_opt_bool(opts, "draft", false)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let def = resolve_collection(reg, collection)?;

    // Find matching documents. We do NOT inject `_status` here — bulk ops
    // operate on the raw query shape Lua provided. The validator still
    // rejects user-supplied `_*` filters as defense-in-depth; if a hook
    // author actually needs to target drafts, they pass the data shape
    // explicitly via the `draft` opt and the matching user filters.
    let (mut find_query, _) = lua_table_to_find_query(query_table)?;
    normalize_filter_fields(&mut find_query.filters, &def.fields);
    validate_user_filters(&find_query.filters).map_err(|e| RuntimeError(format!("{e}")))?;

    enforce_access(
        lua,
        &EnforceAccessParams {
            slug: collection,
            override_access,
            access_fn: def.access.update.as_deref(),
            id: None,
            deny_msg: "Update access denied",
            injecting_status: false,
        },
        &mut find_query.filters,
    )?;

    let mut find_all = FindQuery::new();
    find_all.filters = find_query.filters;

    // Internal batch lookup for bulk mutation — not a user-facing read.
    let docs = query::find(conn, collection, &def, &find_all, locale_ctx.as_ref())
        .map_err(|e| RuntimeError(format!("find error: {e:#}")))?;

    let (hooks_enabled, _guard) = check_hook_depth(lua, run_hooks, collection, "update_many");

    let data = lua_table_to_hashmap(data_table)?;

    if def.is_auth_collection() && data.contains_key("password") {
        return Err(RuntimeError(
            "Cannot set password via update_many. Use single update instead.".into(),
        ));
    }

    let join_data = lua_table_to_json_map(lua, data_table)?;

    let r = reg
        .read()
        .map_err(|e| RuntimeError(format!("Registry lock: {e:#}")))?;

    let write_hooks = LuaWriteHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(override_access)
        .registry(Some(&r))
        .hooks_enabled(hooks_enabled)
        .run_validation(run_hooks)
        .build();

    let ctx = service::ServiceContext::collection(collection, &def)
        .conn(conn)
        .write_hooks(&write_hooks)
        .user(user.as_ref())
        .override_access(override_access)
        .build();

    let mut modified = 0i64;

    for doc in &docs {
        let input = WriteInput::builder(data.clone(), &join_data)
            .locale_ctx(locale_ctx.as_ref())
            .draft(draft)
            .ui_locale(ui_locale.clone())
            .build();

        service::update_many_single_core(&ctx, &doc.id, input, lc)
            .map_err(|e| RuntimeError(format!("{e:#}")))?;

        modified += 1;
    }

    let result = lua.create_table()?;

    result.set("modified", modified)?;

    Ok(result)
}

/// Register `crap.collections.update_many(collection, query, data, opts?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_update_many(
    lua: &Lua,
    table: &Table,
    registry: SharedRegistry,
    locale_config: &LocaleConfig,
) -> anyhow::Result<()> {
    let lc = locale_config.clone();
    let update_many_fn = lua.create_function(
        move |lua,
              (collection, query_table, data_table, opts): (
            String,
            Table,
            Table,
            Option<Table>,
        )| {
            update_many_documents(
                lua,
                &registry,
                &lc,
                &collection,
                &query_table,
                &data_table,
                &opts,
            )
        },
    )?;

    table.set("update_many", update_many_fn)?;

    Ok(())
}
