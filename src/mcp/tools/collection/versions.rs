//! Version tools for collections: list versions and restore a version.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, json, to_string_pretty, to_value};
use tracing::info;

use crate::{
    config::CrapConfig,
    core::Registry,
    db::DbPool,
    service::{list_versions, restore_collection_version},
};

use super::helpers::doc_to_json;

/// Execute `list_versions` — list version snapshots for a document.
pub(in crate::mcp::tools) fn exec_list_versions(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
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

    let (versions, total) = list_versions(&conn, slug, id, limit, offset)?;

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

    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let doc = restore_collection_version(&tx, slug, def, id, version_id, &config.locale)?;

    tx.commit().context("Commit transaction")?;

    info!("MCP restore_version {}: {} -> {}", slug, id, version_id);

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
