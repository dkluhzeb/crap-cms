//! Execute `unpublish` — unpublish a versioned document.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    core::Registry, db::DbPool, hooks::HookRunner, mcp::tools::collection::helpers::doc_to_json,
    service::unpublish_document,
};

/// Execute `unpublish` — set a document to draft status.
pub(in crate::mcp::tools) fn exec_unpublish(
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

    let doc = unpublish_document(pool, runner, slug, id, def, None)?;

    info!("MCP unpublish {}: {}", slug, id);

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
