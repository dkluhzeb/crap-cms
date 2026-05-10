//! Version tools for collections: list versions and restore a version.

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::{Value, to_string_pretty, to_value};
use tracing::info;

use crate::{
    db::query::PaginationResult,
    mcp::tools::ToolExecCtx,
    service::{
        ListVersionsInput, RunnerReadHooks, ServiceContext, list_versions,
        restore_collection_version,
    },
};

use super::helpers::doc_to_json;

/// Shape returned to the MCP client for a `list_versions` tool call.
#[derive(Serialize)]
struct ListVersionsResponse<'a> {
    versions: Vec<Value>,
    pagination: &'a PaginationResult,
}

/// Execute `list_versions` — list version snapshots for a document.
pub(in crate::mcp::tools) fn exec_list_versions(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let def = ctx
        .registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let limit = args.get("limit").and_then(|v| v.as_i64());
    let offset = args.get("offset").and_then(|v| v.as_i64());

    // MCP operates with full access — override access checks
    let conn = ctx.pool.get().context("DB connection")?;
    let hooks = RunnerReadHooks::new(ctx.runner, &conn);
    let svc_ctx = ServiceContext::collection(slug, def)
        .conn(&conn)
        .read_hooks(&hooks)
        .override_access(true)
        .build();

    let input = ListVersionsInput::builder(id)
        .limit(limit)
        .offset(offset)
        .build();

    let result = list_versions(&svc_ctx, &input)?;

    let versions: Vec<Value> = result
        .docs
        .iter()
        .map(|v| to_value(v).unwrap_or(Value::Null))
        .collect();

    let response = ListVersionsResponse {
        versions,
        pagination: &result.pagination,
    };

    Ok(to_string_pretty(&response)?)
}

/// Execute `restore_version` — restore a document to a specific version.
pub(in crate::mcp::tools) fn exec_restore_version(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let version_id = args
        .get("version_id")
        .and_then(|v| v.as_str())
        .context("Missing 'version_id' argument")?;
    let def = ctx
        .registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let svc_ctx = ServiceContext::collection(slug, def)
        .pool(ctx.pool)
        .runner(ctx.runner)
        .override_access(true)
        .event_transport(ctx.event_transport.clone())
        .cache(ctx.cache.clone())
        .build();

    let doc = restore_collection_version(&svc_ctx, id, version_id, &ctx.config.locale)?;

    info!(
        "MCP restore_version {}: {} -> {} [client={}]",
        slug, id, version_id, ctx.client_label
    );

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
