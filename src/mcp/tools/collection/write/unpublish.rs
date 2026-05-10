//! Execute `unpublish` — unpublish a versioned document.

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    mcp::tools::{ToolExecCtx, collection::helpers::doc_to_json},
    service::{ServiceContext, unpublish_document},
};

/// Execute `unpublish` — set a document to draft status.
pub(in crate::mcp::tools) fn exec_unpublish(
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
        .cache(ctx.cache.clone())
        // Required so the raw read inside `unpublish_document_in_conn` builds
        // a default `LocaleContext` for collections with localized fields.
        // Without this, the SELECT references bare column names that
        // don't exist when locales are enabled.
        .locale_config(Some(&ctx.config.locale))
        .build();

    let doc = unpublish_document(&svc_ctx, id)?;

    info!(
        "MCP unpublish {}: {} [client={}]",
        slug, id, ctx.client_label
    );

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
