//! Registration of `crap.collections.find` Lua function.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table};

use crate::{
    config::{LocaleConfig, PaginationConfig},
    core::{CollectionDefinition, Registry},
    db::{
        FindQuery, LocaleContext,
        query::{self, filter::normalize_filter_fields},
    },
    hooks::{
        lifecycle::converters::{
            find_result_to_lua, lua_table_to_find_query, pagination_result_to_lua_table,
        },
        lua_api::crud::{
            get_tx_conn,
            helpers::{
                get_opt_bool, get_opt_string, hook_populate_singleflight, hook_ui_locale,
                hook_user, resolve_collection,
            },
        },
    },
    service::{FindDocumentsInput, LuaReadHooks, ServiceContext, find_documents},
};

/// Parameters for the find operation, capturing all pre-cloned config values.
struct FindParams {
    default: i64,
    max: i64,
    cursor: bool,
}

// Note: `override_access` is parsed from opts and passed to `LuaReadHooks`
// (which passes it to the service layer's `check_access`).

/// Build the `FindQuery` from the Lua table, applying pagination + normalizing
/// filter field paths.
///
/// Produces a *user* query — system filters (`_status`, `_deleted_at`) are
/// injected by `service::find_documents` based on the typed flags.
fn prepare_find_query(
    params: &FindParams,
    def: &CollectionDefinition,
    query_table: Option<&Table>,
) -> LuaResult<FindQuery> {
    let (mut fq, lua_page) = match query_table {
        Some(qt) => lua_table_to_find_query(qt)?,
        None => (FindQuery::default(), None),
    };

    fq.limit = Some(query::apply_pagination_limits(
        fq.limit,
        params.default,
        params.max,
    ));

    if let Some(p) = lua_page {
        let clamped = fq.limit.unwrap_or(params.default);
        fq.offset = Some((p.max(1) - 1) * clamped);
    }

    if !params.cursor {
        fq.after_cursor = None;
        fq.before_cursor = None;
    }

    normalize_filter_fields(&mut fq.filters, &def.fields);

    Ok(fq)
}

/// Core logic for `crap.collections.find`.
fn find_inner(
    lua: &Lua,
    reg: &Registry,
    lc: &LocaleConfig,
    params: &FindParams,
    collection: &str,
    query_table: Option<&Table>,
) -> LuaResult<Table> {
    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let depth: i32 = query_table
        .and_then(|qt| qt.get::<i32>("depth").ok())
        .unwrap_or(0)
        .clamp(0, 10);
    let locale_str = get_opt_string(query_table, "locale");
    let locale_ctx = LocaleContext::from_locale_string(locale_str.as_deref(), lc)
        .map_err(|e| RuntimeError(e.to_string()))?;
    let override_access = get_opt_bool(query_table, "overrideAccess", false);
    let draft = get_opt_bool(query_table, "draft", false);
    let trash = get_opt_bool(query_table, "trash", false);
    let def = resolve_collection(reg, collection)?;

    let mut find_query = prepare_find_query(params, &def, query_table)?;

    let is_trash = trash && def.soft_delete;

    // Default sort for trash listings is a presentation concern — keep here.
    if is_trash && find_query.order_by.is_none() {
        find_query.order_by = Some("-_deleted_at".to_string());
    }

    let hooks = LuaReadHooks::builder(lua)
        .user(user.as_ref())
        .ui_locale(ui_locale.as_deref())
        .override_access(override_access)
        .build();

    let ctx = ServiceContext::collection(collection, &def)
        .conn(conn)
        .read_hooks(&hooks)
        .user(user.as_ref())
        .override_access(override_access)
        .build();

    let input = FindDocumentsInput::builder(&find_query)
        .depth(depth)
        .locale_ctx(locale_ctx.as_ref())
        .registry(Some(reg))
        .select(find_query.select.as_deref())
        .cursor_enabled(params.cursor)
        .trash(is_trash)
        .include_drafts(draft)
        .singleflight(hook_populate_singleflight(lua))
        .build();

    let result = find_documents(&ctx, &input).map_err(|e| RuntimeError(format!("{e}")))?;

    let pagination = pagination_result_to_lua_table(lua, &result.pagination)?;
    find_result_to_lua(lua, &result.docs, pagination)
}

/// Register `crap.collections.find(collection, query?)`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_find(
    lua: &Lua,
    table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
    pagination_config: &PaginationConfig,
) -> Result<()> {
    let lc = locale_config.clone();
    let params = FindParams {
        default: pagination_config.default_limit,
        max: pagination_config.max_limit,
        cursor: pagination_config.is_cursor(),
    };

    let find_fn = lua.create_function(
        move |lua, (collection, query_table): (String, Option<Table>)| {
            find_inner(
                lua,
                &registry,
                &lc,
                &params,
                &collection,
                query_table.as_ref(),
            )
        },
    )?;

    table.set("find", find_fn)?;
    Ok(())
}
