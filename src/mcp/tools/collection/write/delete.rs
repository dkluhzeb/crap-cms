//! Execute `delete` — delete a document by ID.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use tracing::info;

use crate::{
    core::Registry,
    db::DbPool,
    hooks::HookRunner,
    service::{ServiceContext, delete_document},
};

/// Execute `delete` — delete a document by ID.
pub(in crate::mcp::tools) fn exec_delete(
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
    delete_document(&ctx, id, None, None)?;

    info!("MCP delete {}: {}", slug, id);

    Ok(json!({ "deleted": id }).to_string())
}
