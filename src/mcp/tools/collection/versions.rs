//! Version tools for collections: list versions and restore a version.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, json, to_string_pretty, to_value};
use tracing::info;

use crate::{
    config::CrapConfig,
    core::Registry,
    db::DbPool,
    hooks::HookRunner,
    service::{
        ListVersionsInput, RunnerReadHooks, ServiceContext, list_versions,
        restore_collection_version,
    },
};

use super::helpers::doc_to_json;

/// Execute `list_versions` — list version snapshots for a document.
pub(in crate::mcp::tools) fn exec_list_versions(
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

    let limit = args.get("limit").and_then(|v| v.as_i64());
    let offset = args.get("offset").and_then(|v| v.as_i64());

    // MCP operates with full access — override access checks
    let conn = pool.get().context("DB connection")?;
    let hooks = RunnerReadHooks::new(runner, &conn);
    let ctx = ServiceContext::collection(slug, def)
        .conn(&conn)
        .read_hooks(&hooks)
        .override_access(true)
        .build();

    let input = ListVersionsInput::builder(id)
        .limit(limit)
        .offset(offset)
        .build();

    let result = list_versions(&ctx, &input)?;

    let version_values: Vec<Value> = result
        .docs
        .iter()
        .map(|v| to_value(v).unwrap_or(Value::Null))
        .collect();

    let output = json!({
        "versions": version_values,
        "pagination": to_value(&result.pagination)?,
    });

    Ok(to_string_pretty(&output)?)
}

/// Execute `restore_version` — restore a document to a specific version.
pub(in crate::mcp::tools) fn exec_restore_version(
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
    let version_id = args
        .get("version_id")
        .and_then(|v| v.as_str())
        .context("Missing 'version_id' argument")?;
    let def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let ctx = ServiceContext::collection(slug, def)
        .pool(pool)
        .runner(runner)
        .override_access(true)
        .build();

    let doc = restore_collection_version(&ctx, id, version_id, &config.locale)?;

    info!("MCP restore_version {}: {} -> {}", slug, id, version_id);

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
