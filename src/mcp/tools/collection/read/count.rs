//! Execute `count` — count documents matching filters.

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::{Value, to_string_pretty};

use crate::{
    mcp::tools::{ToolExecCtx, collection::helpers::parse_where_filters},
    service::{
        CountDocumentsInput, RunnerReadHooks, ServiceContext, ServiceError, count_documents,
    },
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
    let def = ctx
        .registry
        .collections
        .get(slug)
        .context("Collection not found")?;
    let conn = ctx.pool.get().context("DB connection")?;

    let filters = parse_where_filters(args)?;
    let include_drafts = args
        .get("draft")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let trash = args
        .get("trash")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let hooks = RunnerReadHooks::new(ctx.runner, &conn, None, None);
    let svc_ctx = ServiceContext::collection(slug, def)
        .pool(ctx.pool)
        .conn(&conn)
        .read_hooks(&hooks)
        .override_access(true)
        .build();

    let input = CountDocumentsInput::builder(&filters)
        .include_drafts(include_drafts)
        .trash(trash)
        .build();

    let count = count_documents(&svc_ctx, &input).map_err(ServiceError::into_anyhow)?;

    Ok(to_string_pretty(&CountResponse { count })?)
}
