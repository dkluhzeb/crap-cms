//! Registration of `crap.collections.find` Lua function.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, FromLua, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::{Deserialize, Serialize};

use crate::{
    config::{DepthConfig, LocaleConfig, PaginationConfig},
    core::{CollectionDefinition, Document, Registry},
    db::{
        FindQuery, LocaleContext, PaginationResult,
        query::{self, filter::normalize_filter_fields},
    },
    hooks::lua_api::crud::{
        filter::convert_where_clause,
        get_tx_conn,
        helpers::{hook_populate_singleflight, hook_ui_locale, hook_user, resolve_collection},
    },
    service::{FindDocumentsInput, LuaReadHooks, ServiceContext, find_documents},
    typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Query passed to `crap.collections.find(collection, query)`.
#[derive(Debug, Default, Deserialize, LuaAnnotation)]
#[serde(default, deny_unknown_fields)]
#[lua(class = "crap.FindQuery")]
pub(crate) struct FindQueryInput {
    /// Field filters. String values = `equals`, table values =
    /// operators. Use `["or"]` for OR groups. Keys support dot
    /// notation for nested fields (`"seo.title"`, `"variants.color"`,
    /// `"tags.id"`).
    #[serde(rename = "where")]
    #[lua(
        rename = "where",
        ty = "table<string, crap.FilterValue | crap.OrCondition[]>",
        optional
    )]
    pub(crate) where_: Option<HashMap<String, serde_json::Value>>,
    /// Sort field (prefix with `"-"` for descending).
    #[lua(optional)]
    pub(crate) order_by: Option<String>,
    /// Max results to return.
    #[lua(optional)]
    pub(crate) limit: Option<i64>,
    /// Page number (1-based). Converted to offset internally.
    #[lua(optional)]
    pub(crate) page: Option<i64>,
    /// Number of results to skip (alias for `page`).
    #[lua(optional)]
    pub(crate) offset: Option<i64>,
    /// Population depth for relationship fields (default: `0`).
    #[lua(optional)]
    pub(crate) depth: Option<i32>,
    /// Locale code for localized fields (`"en"`, `"de"`, `"all"`).
    #[lua(optional)]
    pub(crate) locale: Option<String>,
    /// Fields to return. Nil/empty = all fields.
    #[lua(ty = "string[]", optional)]
    pub(crate) select: Option<Vec<String>>,
    /// Include draft documents (versioned collections only).
    #[lua(optional)]
    pub(crate) draft: Option<bool>,
    /// Skip access control checks (default: `false`).
    #[serde(rename = "overrideAccess")]
    #[lua(rename = "overrideAccess", optional)]
    pub(crate) override_access: Option<bool>,
    /// Include soft-deleted documents (trash listings).
    #[lua(optional)]
    pub(crate) trash: Option<bool>,
    /// Forward cursor token (from previous response's `endCursor`).
    #[lua(optional)]
    pub(crate) after_cursor: Option<String>,
    /// Backward cursor token (from previous response's `startCursor`).
    #[lua(optional)]
    pub(crate) before_cursor: Option<String>,
    /// FTS5 full-text search query.
    #[lua(optional)]
    pub(crate) search: Option<String>,
}

impl FromLua for FindQueryInput {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        match value {
            Value::Nil => Ok(Self::default()),
            other => lua.from_value(other),
        }
    }
}

impl FindQueryInput {
    /// Project this input into the runtime `(FindQuery, page)` pair.
    /// `page` is returned separately so the handler can apply
    /// pagination-config limits before computing the offset.
    pub(crate) fn into_find_query(self) -> LuaResult<(FindQuery, Option<i64>)> {
        let filters = match self.where_ {
            Some(w) => convert_where_clause(w)?,
            None => Vec::new(),
        };

        let after_cursor = self
            .after_cursor
            .as_deref()
            .map(crate::db::query::cursor::CursorData::decode)
            .transpose()
            .map_err(|e| RuntimeError(format!("Invalid cursor: {e:#}")))?;
        let before_cursor = self
            .before_cursor
            .as_deref()
            .map(crate::db::query::cursor::CursorData::decode)
            .transpose()
            .map_err(|e| RuntimeError(format!("Invalid cursor: {e:#}")))?;

        let offset = if self.page.is_some() {
            None
        } else {
            self.offset
        };

        let fq = FindQuery::builder()
            .filters(filters)
            .order_by(self.order_by)
            .limit(self.limit)
            .offset(offset)
            .select(self.select)
            .after_cursor(after_cursor)
            .before_cursor(before_cursor)
            .search(self.search)
            .build();

        Ok((fq, self.page))
    }
}

/// Result of `crap.collections.find(...)`. Constructed by the handler
/// and serialized via `LuaSerdeExt::to_value` — the Lua-side
/// `crap.FindResult` class is derived from this struct.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.FindResult")]
pub(crate) struct FindResult<'a> {
    /// Matching documents.
    #[lua(ty = "crap.Document[]")]
    documents: &'a [Document],
    /// Pagination metadata.
    #[lua(ty = "crap.PaginationInfo")]
    pagination: &'a PaginationResult,
}

/// Parameters for the find operation, capturing all pre-cloned config values.
#[derive(Clone)]
pub(crate) struct FindParams {
    default: i64,
    max: i64,
    cursor: bool,
    /// Upper bound for relationship-population `depth`, from `[depth] max_depth`.
    max_depth: i32,
}

/// State threaded into `crap.collections.find`.
pub(crate) struct CollectionsFindState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) locale_config: LocaleConfig,
    pub(crate) params: FindParams,
}

/// Find documents matching a query. Returns documents and total count.
/// Inside hooks, runs within the parent operation's transaction.
#[lua_fn(path = "crap.collections.find", returns = "crap.FindResult", auto_tx)]
fn collections_find(
    state: &CollectionsFindState,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] collection: String,
    #[lua(ty = "crap.FindQuery", doc = "Optional query parameters.")] query: Option<FindQueryInput>,
) -> LuaResult<Table> {
    find_inner(
        lua,
        &state.registry,
        &state.locale_config,
        &state.params,
        &collection,
        query.unwrap_or_default(),
    )
}

lua_table! {
    name: crap_collections_find,
    path: "crap.collections",
    state: CollectionsFindState,
    fns: [collections_find],
}

/// Register `crap.collections.find(collection, query?)`. Parent
/// `crap.collections` must already exist.
#[cfg(not(tarpaulin_include))]
pub(crate) fn register_find(
    lua: &Lua,
    _table: &Table,
    registry: Arc<Registry>,
    locale_config: &LocaleConfig,
    pagination_config: &PaginationConfig,
    depth_config: &DepthConfig,
) -> Result<()> {
    let params = FindParams {
        default: pagination_config.default_limit,
        max: pagination_config.max_limit,
        cursor: pagination_config.is_cursor(),
        max_depth: depth_config.max_depth,
    };

    register_crap_collections_find(
        lua,
        CollectionsFindState {
            registry,
            locale_config: locale_config.clone(),
            params,
        },
    )?;
    Ok(())
}

/// Apply pagination limits + cursor gating + filter normalization to
/// the typed `FindQuery` produced by `FindQueryInput`. System filters
/// (`_status`, `_deleted_at`) are injected by `service::find_documents`
/// based on the typed flags.
fn finalize_find_query(
    params: &FindParams,
    def: &CollectionDefinition,
    mut fq: FindQuery,
    lua_page: Option<i64>,
) -> FindQuery {
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
    fq
}

/// Core logic for `crap.collections.find`.
fn find_inner(
    lua: &Lua,
    reg: &Registry,
    lc: &LocaleConfig,
    params: &FindParams,
    collection: &str,
    query: FindQueryInput,
) -> LuaResult<Table> {
    let conn = get_tx_conn(lua)?;

    let user = hook_user(lua);
    let ui_locale = hook_ui_locale(lua);
    let depth = query.depth.unwrap_or(0).clamp(0, params.max_depth);
    let locale_ctx = LocaleContext::from_locale_string(query.locale.as_deref(), lc)
        .map_err(|e| RuntimeError(e.to_string()))?;
    let override_access = query.override_access.unwrap_or(false);
    let draft = query.draft.unwrap_or(false);
    let trash = query.trash.unwrap_or(false);
    let def = resolve_collection(reg, collection)?;

    let (raw_fq, lua_page) = query.into_find_query()?;
    let mut find_query = finalize_find_query(params, &def, raw_fq, lua_page);

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

    let find_result = FindResult {
        documents: &result.docs,
        pagination: &result.pagination,
    };
    let Value::Table(tbl) = lua.to_value(&find_result)? else {
        return Err(RuntimeError(
            "FindResult did not serialize to a table".into(),
        ));
    };
    Ok(tbl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse_find(json_obj: serde_json::Value) -> (FindQuery, Option<i64>) {
        let input: FindQueryInput = serde_json::from_value(json_obj).unwrap();
        input.into_find_query().unwrap()
    }

    #[test]
    fn find_query_empty_input_produces_default() {
        let (fq, page) = parse_find(json!({}));
        assert!(fq.filters.is_empty());
        assert!(fq.order_by.is_none());
        assert!(fq.limit.is_none());
        assert!(fq.offset.is_none());
        assert!(page.is_none());
    }

    #[test]
    fn find_query_pagination_with_offset() {
        let (fq, page) = parse_find(json!({ "limit": 10, "offset": 20 }));
        assert_eq!(fq.limit, Some(10));
        assert_eq!(fq.offset, Some(20));
        assert!(page.is_none());
    }

    #[test]
    fn find_query_page_zeroes_offset_for_handler_pagination() {
        // `page` is returned separately so the handler can apply
        // pagination-config limits before computing the offset.
        let (fq, page) = parse_find(json!({ "limit": 10, "page": 3, "offset": 99 }));
        assert!(fq.offset.is_none(), "page should suppress offset");
        assert_eq!(page, Some(3));
    }

    #[test]
    fn find_query_select_is_typed_string_array() {
        let (fq, _) = parse_find(json!({ "select": ["title", "slug"] }));
        assert_eq!(fq.select.as_ref().unwrap(), &["title", "slug"]);
    }

    #[test]
    fn find_query_simple_where_filter() {
        let (fq, _) = parse_find(json!({ "where": { "status": "published" } }));
        assert_eq!(fq.filters.len(), 1);
    }

    #[test]
    fn find_query_invalid_cursor_errors() {
        let input: FindQueryInput = serde_json::from_value(json!({
            "after_cursor": "this-is-not-base64",
        }))
        .unwrap();
        let err = input.into_find_query().unwrap_err();
        assert!(err.to_string().contains("Invalid cursor"), "got: {err}");
    }

    #[test]
    fn find_query_unknown_top_level_field_errors() {
        // `deny_unknown_fields` rejects user typos at the top level.
        let err = serde_json::from_value::<FindQueryInput>(json!({
            "limt": 10, // typo
        }))
        .unwrap_err();
        assert!(err.to_string().contains("limt"), "got: {err}");
    }

    #[test]
    fn find_query_override_access_camel_case() {
        // Lua-side `overrideAccess` deserializes into `override_access`.
        let input: FindQueryInput =
            serde_json::from_value(json!({ "overrideAccess": true })).unwrap();
        assert_eq!(input.override_access, Some(true));
    }
}
