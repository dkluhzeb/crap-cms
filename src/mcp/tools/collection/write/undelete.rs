//! Execute `undelete` — restore a soft-deleted document.

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    mcp::tools::ToolExecCtx,
    service::{ServiceContext, undelete_document},
};

#[derive(Serialize)]
struct RestoredResponse<'a> {
    restored: &'a str,
}

/// Execute `undelete` — restore a soft-deleted document.
pub(in crate::mcp::tools) fn exec_undelete(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let def = ctx
        .infra
        .registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let svc_ctx = ServiceContext::collection(slug, def)
        .pool(&ctx.infra.pool)
        .runner(&ctx.infra.hook_runner)
        .override_access(true)
        .event_transport(ctx.infra.event_transport.clone())
        .invalidation_transport(Some(ctx.infra.invalidation_transport.clone()))
        .cache(Some(ctx.infra.cache.clone()))
        .build();

    undelete_document(&svc_ctx, id)?;

    info!(
        "MCP undelete {}: {} [client={}]",
        slug, id, ctx.client_label
    );

    Ok(to_string_pretty(&RestoredResponse { restored: id })?)
}
