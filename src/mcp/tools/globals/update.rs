//! Execute `update_global` — update a global document.

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{doc_to_json, extract_data_from_args},
    },
    service::{ServiceContext, WriteInput, update_global_document},
};

/// Execute `update_global` — update a global document.
pub(in crate::mcp::tools) fn exec_update_global(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let def = ctx.registry.globals.get(slug).context("Global not found")?;

    let data = extract_data_from_args(args, &[]);

    let svc_ctx = ServiceContext::global(slug, def)
        .pool(ctx.pool)
        .runner(ctx.runner)
        .override_access(true)
        .event_transport(ctx.event_transport.clone())
        .cache(ctx.cache.clone())
        .build();

    let (doc, _ctx) = update_global_document(&svc_ctx, WriteInput::builder(data).build())?;

    info!("MCP update global: {} [client={}]", slug, ctx.client_label);

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
