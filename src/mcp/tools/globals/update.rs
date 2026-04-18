//! Execute `update_global` — update a global document.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    core::{Registry, cache::SharedCache, event::SharedEventTransport},
    db::DbPool,
    hooks::HookRunner,
    mcp::tools::collection::helpers::{doc_to_json, extract_data_from_args},
    service::{ServiceContext, WriteInput, update_global_document},
};

/// Execute `update_global` — update a global document.
pub(in crate::mcp::tools) fn exec_update_global(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
) -> Result<String> {
    let def = registry.globals.get(slug).context("Global not found")?;

    let (data, join_data) = extract_data_from_args(args, &[]);

    let ctx = ServiceContext::global(slug, def)
        .pool(pool)
        .runner(runner)
        .override_access(true)
        .event_transport(event_transport)
        .cache(cache)
        .build();

    let (doc, _ctx) = update_global_document(&ctx, WriteInput::builder(data, &join_data).build())?;

    info!("MCP update global: {}", slug);

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
