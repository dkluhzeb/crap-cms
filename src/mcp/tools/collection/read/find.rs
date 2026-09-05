//! Execute `find` — paginated query with filters, search, and population.
//!
//! Codec over [`op::run`] with [`Principal::Override`]: decode the tool
//! args into the canonical [`FindQuery`] + [`FindArgs`], dispatch, encode.
//! The trash downgrade, trash default order, and query-field validation live
//! in the operation body.

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde_json::{Value, to_string_pretty};

use crate::{
    db::{FindQuery, LocaleContext, query, query::PaginationResult},
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{doc_to_json, parse_select, parse_where_filters},
    },
    service::op::{self, Find, FindArgs, Principal, TargetRef},
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
    let limit = args.get("limit").and_then(serde_json::Value::as_i64);
    let page = args.get("page").and_then(serde_json::Value::as_i64);
    let after_cursor = args.get("after_cursor").and_then(|v| v.as_str());
    let before_cursor = args.get("before_cursor").and_then(|v| v.as_str());

    let pg_ctx = query::PaginationCtx::from_config(&ctx.config.pagination);
    let pagination = pg_ctx
        .validate(limit, page, after_cursor, before_cursor)
        .map_err(|e| anyhow!(e))?;

    let order_by = args
        .get("order_by")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
    let search = args
        .get("search")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;

    // Field projection, parity with gRPC/Lua `select`.
    let select = parse_select(args);

    // MCP requests outside i32 range can't be valid populate depths; treat
    // them as 0 (no population) before applying max_depth.
    let requested = args
        .get("depth")
        .and_then(serde_json::Value::as_i64)
        .and_then(|d| i32::try_from(d).ok());
    let depth = query::clamp_depth(
        requested,
        ctx.config.depth.default_depth,
        ctx.config.depth.max_depth,
    );

    let trash = args
        .get("trash")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let include_drafts = args
        .get("draft")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let offset = (!pagination.has_cursor()).then_some(pagination.offset);

    let fq = FindQuery::builder()
        .filters(parse_where_filters(args)?)
        .order_by(order_by)
        .limit(Some(pagination.limit))
        .offset(offset)
        .after_cursor(pagination.after_cursor.clone())
        .before_cursor(pagination.before_cursor.clone())
        .search(search)
        .select(select)
        .build();

    let op_args = FindArgs::builder(fq)
        .depth(depth)
        .locale_ctx(locale_ctx)
        .cursor_enabled(ctx.config.pagination.is_cursor())
        .trash(trash)
        .include_drafts(include_drafts)
        .build();

    let result = op::run::<Find>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::collection(slug),
        op_args,
    )
    .map_err(|e| e.into_service_error().into_anyhow_scrubbed())?;

    let docs: Vec<Value> = result.docs.iter().map(doc_to_json).collect();
    let response = FindResponse {
        docs,
        pagination: &result.pagination,
    };
    Ok(to_string_pretty(&response)?)
}
