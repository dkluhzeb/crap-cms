//! Execute `unpublish` — unpublish a versioned document.
//!
//! Codec over [`op::run`] with [`Principal::Override`]. The context comes
//! fully assembled from the operation core (locale config included, so the
//! raw read inside the unpublish body resolves localized columns).

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{doc_to_json, events_flag},
    },
    service::op::{self, Principal, TargetRef, Unpublish, UnpublishArgs},
};

/// Execute `unpublish` — set a document to draft status.
pub(in crate::mcp::tools) fn exec_unpublish(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;

    let doc = op::run::<Unpublish>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::collection(slug),
        UnpublishArgs::new(id).events(events_flag(args)),
    )
    .map_err(|e| e.into_service_error().into_anyhow())?;

    info!(
        "MCP unpublish {}: {} [client={}]",
        slug, id, ctx.client_label
    );

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
