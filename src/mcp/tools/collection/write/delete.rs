//! Execute `delete` — delete a document by ID.
//!
//! Codec over [`op::run`] with [`Principal::Override`]. `force_hard_delete`
//! is expressed by the operation body via `adjust_collection_def` — the
//! definition-clone trick previously copy-pasted on every surface.

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    mcp::tools::{ToolExecCtx, collection::helpers::events_flag},
    service::op::{self, Delete, DeleteArgs, Principal, TargetRef},
};

#[derive(Serialize)]
struct DeletedResponse<'a> {
    deleted: &'a str,
}

/// Execute `delete` — delete a document by ID.
pub(in crate::mcp::tools) fn exec_delete(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let force_hard_delete = args
        .get("force_hard_delete")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let op_args = DeleteArgs::builder(id)
        .force_hard_delete(force_hard_delete)
        .events(events_flag(args))
        .build();

    op::run::<Delete>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::collection(slug),
        op_args,
    )
    .map_err(|e| e.into_service_error().into_anyhow())?;

    info!("MCP delete {}: {} [client={}]", slug, id, ctx.client_label);

    Ok(to_string_pretty(&DeletedResponse { deleted: id })?)
}
