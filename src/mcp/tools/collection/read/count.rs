//! Execute `count` — count documents matching filters.
//!
//! Codec over [`op::run`] with [`Principal::Override`].

use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, to_string_pretty};

use crate::{
    db::LocaleContext,
    mcp::tools::{ToolExecCtx, collection::helpers::parse_where_filters},
    service::op::{self, Count, CountArgs, Principal, TargetRef},
};

/// Shape returned to the MCP client for a `count` tool call.
#[derive(Serialize)]
struct CountResponse {
    count: i64,
}

/// Execute `count` — count documents matching filters.
pub(in crate::mcp::tools) fn exec_count(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let include_drafts = args
        .get("draft")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let trash = args
        .get("trash")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    // Locale + search, parity with gRPC/Lua count (and MCP's own find) — a
    // count must be able to summarize the exact same query as its list.
    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;
    let search = args
        .get("search")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);

    let op_args = CountArgs::builder(parse_where_filters(args)?)
        .include_drafts(include_drafts)
        .trash(trash)
        .locale_ctx(locale_ctx)
        .search(search)
        .build();

    let count = op::run::<Count>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::collection(slug),
        op_args,
    )
    .map_err(|e| e.into_service_error().into_anyhow())?;

    Ok(to_string_pretty(&CountResponse { count })?)
}
