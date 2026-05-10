//! Execute `delete` — delete a document by ID.

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    mcp::tools::ToolExecCtx,
    service::{ServiceContext, delete_document},
};

#[derive(Serialize)]
struct DeletedResponse<'a> {
    deleted: &'a str,
}

/// Execute `delete` — delete a document by ID.
pub(in crate::mcp::tools) fn exec_delete(
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

    let svc_ctx = ServiceContext::collection(slug, def)
        .pool(ctx.pool)
        .runner(ctx.runner)
        .override_access(true)
        .event_transport(ctx.event_transport.clone())
        .invalidation_transport(ctx.invalidation_transport.clone())
        .cache(ctx.cache.clone())
        .build();
    delete_document(&svc_ctx, id, None, None)?;

    info!("MCP delete {}: {} [client={}]", slug, id, ctx.client_label);

    Ok(to_string_pretty(&DeletedResponse { deleted: id })?)
}
