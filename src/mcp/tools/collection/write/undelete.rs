//! Execute `undelete` — restore a soft-deleted document.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use tracing::info;

use crate::{
    core::Registry,
    db::DbPool,
    hooks::HookRunner,
    service::{ServiceContext, undelete_document},
};

/// Execute `undelete` — restore a soft-deleted document.
pub(in crate::mcp::tools) fn exec_undelete(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let ctx = ServiceContext::collection(slug, def)
        .pool(pool)
        .runner(runner)
        .override_access(true)
        .build();

    undelete_document(&ctx, id)?;

    info!("MCP undelete {}: {}", slug, id);

    Ok(json!({ "restored": id }).to_string())
}
