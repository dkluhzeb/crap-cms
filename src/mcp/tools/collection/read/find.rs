//! Execute `find` — paginated query with filters, search, and population.

use anyhow::{Context as _, Result, anyhow};
use serde::Serialize;
use serde_json::{Value, to_string_pretty};

use crate::{
    db::{FindQuery, LocaleContext, query, query::PaginationResult},
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{doc_to_json, parse_where_filters},
    },
    service::{FindDocumentsInput, RunnerReadHooks, ServiceContext, ServiceError, find_documents},
};

/// Shape returned to the MCP client for a `find` tool call.
#[derive(Serialize)]
struct FindResponse<'a> {
    docs: Vec<Value>,
    pagination: &'a PaginationResult,
}

/// Execute `find` — paginated query with filters, search, and population.
pub(in crate::mcp::tools) fn exec_find(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let def = ctx
        .registry
        .collections
        .get(slug)
        .context("Collection not found")?;
    let conn = ctx.pool.get().context("DB connection")?;

    let limit = args.get("limit").and_then(|v| v.as_i64());
    let page = args.get("page").and_then(|v| v.as_i64());
    let after_cursor = args.get("after_cursor").and_then(|v| v.as_str());
    let before_cursor = args.get("before_cursor").and_then(|v| v.as_str());

    let pg_ctx = query::PaginationCtx::new(
        ctx.config.pagination.default_limit,
        ctx.config.pagination.max_limit,
        ctx.config.pagination.is_cursor(),
    );
    let pagination = pg_ctx
        .validate(limit, page, after_cursor, before_cursor)
        .map_err(|e| anyhow!(e))?;

    let order_by = args
        .get("order_by")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let search = args
        .get("search")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;

    let depth = args.get("depth").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let depth = depth.min(ctx.config.depth.max_depth);

    let is_trash = args.get("trash").and_then(|v| v.as_bool()).unwrap_or(false) && def.soft_delete;
    let include_drafts = args.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);

    // Default sort for trash listings is a presentation concern.
    let order_by = order_by
        .clone()
        .or_else(|| is_trash.then(|| "-_deleted_at".to_string()));

    let offset = (!pagination.has_cursor()).then_some(pagination.offset);

    let fq = FindQuery::builder()
        .filters(parse_where_filters(args))
        .order_by(order_by)
        .limit(Some(pagination.limit))
        .offset(offset)
        .after_cursor(pagination.after_cursor.clone())
        .before_cursor(pagination.before_cursor.clone())
        .search(search.clone())
        .build();

    let hooks = RunnerReadHooks::new(ctx.runner, &conn);
    let svc_ctx = ServiceContext::collection(slug, def)
        .pool(ctx.pool)
        .conn(&conn)
        .read_hooks(&hooks)
        .override_access(true)
        .build();

    let input = FindDocumentsInput::builder(&fq)
        .depth(depth)
        .locale_ctx(locale_ctx.as_ref())
        .registry(Some(ctx.registry.as_ref()))
        .cursor_enabled(ctx.config.pagination.is_cursor())
        .trash(is_trash)
        .include_drafts(include_drafts)
        .build();

    let result = find_documents(&svc_ctx, &input).map_err(ServiceError::into_anyhow)?;

    let docs: Vec<Value> = result.docs.iter().map(doc_to_json).collect();
    let response = FindResponse {
        docs,
        pagination: &result.pagination,
    };
    Ok(to_string_pretty(&response)?)
}
