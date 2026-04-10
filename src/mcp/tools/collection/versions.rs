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
    service::{list_versions, restore_collection_version},
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
    let _def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;
    let conn = pool.get().context("DB connection")?;

    let limit = args.get("limit").and_then(|v| v.as_i64());
    let offset = args.get("offset").and_then(|v| v.as_i64());

    // MCP operates with full access — pass None for access_ref and user
    let hooks = crate::service::RunnerReadHooks::new(runner, &conn);
    let (versions, total) = list_versions(&conn, &hooks, slug, id, None, None, limit, offset)?;

    let version_values: Vec<Value> = versions
        .iter()
        .map(|v| to_value(v).unwrap_or(Value::Null))
        .collect();

    let output = json!({
        "versions": version_values,
        "total": total,
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

    let doc = restore_collection_version(
        pool,
        runner,
        slug,
        def,
        id,
        version_id,
        &config.locale,
        None,
        true,
    )?;

    info!("MCP restore_version {}: {} -> {}", slug, id, version_id);

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
