//! Execute `count` — count documents matching filters.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, json};

use crate::{
    core::Registry,
    db::DbPool,
    hooks::HookRunner,
    mcp::tools::collection::helpers::parse_where_filters,
    service::{CountDocumentsInput, RunnerReadHooks, ServiceContext, count_documents},
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
    let ctx = ServiceContext::collection(slug, def)
        .pool(pool)
        .conn(&conn)
        .read_hooks(&hooks)
        .override_access(true)
        .build();

    let input = CountDocumentsInput::builder(&filters)
        .include_deleted(include_deleted)
        .build();

    let count = count_documents(&ctx, &input).map_err(|e| e.into_anyhow())?;

    Ok(json!({ "count": count }).to_string())
}
