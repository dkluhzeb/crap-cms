//! Execute `undelete` — restore a soft-deleted document.
//!
//! Codec over [`op::run`] with [`Principal::Override`].

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    mcp::tools::{ToolExecCtx, collection::helpers::events_flag},
    service::op::{self, Principal, TargetRef, Undelete, UndeleteArgs},
};

#[derive(Serialize)]
struct RestoredResponse<'a> {
    restored: &'a str,
}

/// Execute `undelete` — restore a soft-deleted document.
pub(in crate::mcp::tools) fn exec_undelete(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;

    op::run::<Undelete>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::collection(slug),
        UndeleteArgs::new(id).events(events_flag(args)),
    )
    .map_err(|e| e.into_service_error().into_anyhow())?;

    info!(
        "MCP undelete {}: {} [client={}]",
        slug, id, ctx.client_label
    );

    Ok(to_string_pretty(&RestoredResponse { restored: id })?)
}
