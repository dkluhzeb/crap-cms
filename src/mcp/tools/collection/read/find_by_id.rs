//! Execute `find_by_id` — single document lookup with population.
//!
//! Stage-2 codec: decode the tool args into [`FindByIdArgs`], dispatch
//! through [`op::run`] with [`Principal::Override`] (MCP is a trusted local
//! transport), encode the result. Connection acquisition, hook wiring, and
//! the definition-dependent flag downgrades live in the operation core.

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::{Value, to_string_pretty};

use crate::{
    db::{LocaleContext, query},
    mcp::tools::{ToolExecCtx, collection::helpers::doc_to_json},
    service::op::{self, FindById, FindByIdArgs, Principal, TargetRef},
};

/// Soft "not found" reply for `find_by_id` — distinct from a tool error.
#[derive(Serialize)]
struct NotFoundResponse {
    error: &'static str,
}

/// Execute `find_by_id` — single document lookup with population.
pub(in crate::mcp::tools) fn exec_find_by_id(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;

    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;

    // A depth outside i32 range (or absent) resolves to the configured default;
    // `clamp_depth` then floors negatives at 0 and caps at max_depth — the one
    // shared depth resolver every read surface uses.
    let requested = args
        .get("depth")
        .and_then(serde_json::Value::as_i64)
        .and_then(|d| i32::try_from(d).ok());
    let depth = query::clamp_depth(
        requested,
        ctx.config.depth.default_depth,
        ctx.config.depth.max_depth,
    );

    // Draft/trash view selectors, mirrored from `find`. MCP runs with
    // override_access, so these are view selectors, not a gate.
    let use_draft = args
        .get("draft")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let include_deleted = args
        .get("trash")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let op_args = FindByIdArgs::builder(id)
        .depth(depth)
        .locale_ctx(locale_ctx)
        .use_draft(use_draft)
        .include_deleted(include_deleted)
        .build();

    let doc = op::run::<FindById>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::collection(slug),
        &op_args,
    )
    .map_err(|e| e.into_service_error().into_anyhow())?;

    match doc {
        Some(d) => Ok(to_string_pretty(&doc_to_json(&d))?),
        None => Ok(to_string_pretty(&NotFoundResponse {
            error: "Document not found",
        })?),
    }
}
