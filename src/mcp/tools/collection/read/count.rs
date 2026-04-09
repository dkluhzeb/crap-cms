//! Execute `count` — count documents matching filters.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, json};

use crate::{
    core::Registry,
    db::DbPool,
    hooks::HookRunner,
    mcp::tools::collection::helpers::parse_where_filters,
    service::{RunnerReadHooks, count_documents},
};

/// Execute `count` — count documents matching filters.
pub(in crate::mcp::tools) fn exec_count(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
) -> Result<String> {
    let def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;
    let conn = pool.get().context("DB connection")?;

    let filters = parse_where_filters(args);
    let include_deleted = args.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);

    let hooks = RunnerReadHooks::new(runner, &conn);

    let count = count_documents(
        &conn,
        &hooks,
        slug,
        def,
        &filters,
        None,
        None,
        include_deleted,
        None,
    )
    .map_err(|e| e.into_anyhow())?;

    Ok(json!({ "count": count }).to_string())
}
