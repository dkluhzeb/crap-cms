//! Execute `find_by_id` — single document lookup with population.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, json, to_string_pretty};

use crate::{
    config::CrapConfig,
    core::Registry,
    db::DbPool,
    hooks::HookRunner,
    mcp::tools::collection::helpers::doc_to_json,
    service::{ReadOptions, RunnerReadHooks, find_document_by_id},
};

/// Execute `find_by_id` — single document lookup with population.
pub(in crate::mcp::tools) fn exec_find_by_id(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
    config: &CrapConfig,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;
    let conn = pool.get().context("DB connection")?;

    let depth = args
        .get("depth")
        .and_then(|v| v.as_i64())
        .unwrap_or(config.depth.default_depth as i64) as i32;
    let depth = depth.min(config.depth.max_depth);

    let hooks = RunnerReadHooks::new(runner, &conn);
    let opts = ReadOptions::builder()
        .depth(depth)
        .registry(Some(registry.as_ref()))
        .build();

    let doc =
        find_document_by_id(&conn, &hooks, slug, def, id, &opts).map_err(|e| e.into_anyhow())?;

    match doc {
        Some(d) => Ok(to_string_pretty(&doc_to_json(&d))?),
        None => Ok(json!({ "error": "Document not found" }).to_string()),
    }
}
