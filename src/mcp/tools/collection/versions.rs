//! Version tools for collections: list versions and restore a version.

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::{Value, to_string_pretty, to_value};
use tracing::info;

use crate::{
    db::query::PaginationResult,
    mcp::tools::ToolExecCtx,
    service::op::{
        self, ListVersions, ListVersionsArgs, Principal, RestoreVersion, RestoreVersionArgs,
        TargetRef,
    },
};

use super::helpers::doc_to_json;

/// Shape returned to the MCP client for a `list_versions` tool call.
#[derive(Serialize)]
struct ListVersionsResponse<'a> {
    versions: Vec<Value>,
    pagination: &'a PaginationResult,
}

/// Execute `list_versions` — list version snapshots for a document.
pub(in crate::mcp::tools) fn exec_list_versions(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    // Pure decode — negative limit/offset are floored at the service
    // chokepoint (`service::versions::list_versions`), same for every surface.
    let limit = args.get("limit").and_then(serde_json::Value::as_i64);
    let offset = args.get("offset").and_then(serde_json::Value::as_i64);

    let op_args = ListVersionsArgs::builder(id)
        .limit(limit)
        .offset(offset)
        .build();

    // MCP operates with full access — override access checks.
    let result = op::run::<ListVersions>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::collection(slug),
        op_args,
    )
    .map_err(|e| e.into_service_error().into_anyhow_scrubbed())?;

    let versions: Vec<Value> = result
        .docs
        .iter()
        .map(|v| to_value(v).unwrap_or(Value::Null))
        .collect();

    let response = ListVersionsResponse {
        versions,
        pagination: &result.pagination,
    };

    Ok(to_string_pretty(&response)?)
}

/// Execute `restore_version` — restore a document to a specific version.
pub(in crate::mcp::tools) fn exec_restore_version(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let version_id = args
        .get("version_id")
        .and_then(|v| v.as_str())
        .context("Missing 'version_id' argument")?;
    let doc = op::run::<RestoreVersion>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::collection(slug),
        RestoreVersionArgs::new(id, version_id),
    )
    .map_err(|e| e.into_service_error().into_anyhow_scrubbed())?;

    info!(
        "MCP restore_version {}: {} -> {} [client={}]",
        slug, id, version_id, ctx.client_label
    );

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
