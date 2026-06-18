//! Execute `find_by_id` — single document lookup with population.

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::{Value, to_string_pretty};

use crate::{
    db::LocaleContext,
    mcp::tools::{ToolExecCtx, collection::helpers::doc_to_json},
    service::{FindByIdInput, RunnerReadHooks, ServiceContext, ServiceError, find_document_by_id},
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
    let def = ctx
        .registry
        .collections
        .get(slug)
        .context("Collection not found")?;
    let conn = ctx.pool.get().context("DB connection")?;

    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;

    // MCP requests outside i32 range can't be valid populate depths; clamp
    // to the configured default before applying max_depth.
    let depth_raw = args
        .get("depth")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(i64::from(ctx.config.depth.default_depth));
    let depth = i32::try_from(depth_raw)
        .unwrap_or(ctx.config.depth.default_depth)
        .min(ctx.config.depth.max_depth);

    let hooks = RunnerReadHooks::new(ctx.runner, &conn, None, None).with_override_access();
    let svc_ctx = ServiceContext::collection(slug, def)
        .pool(ctx.pool)
        .conn(&conn)
        .read_hooks(&hooks)
        .override_access(true)
        .build();

    // Mirror `find`: thread the draft/trash view selectors so a single-document
    // lookup can reach the same views the list endpoint exposes. MCP runs with
    // override_access, so these are view selectors, not a gate.
    let use_draft = args
        .get("draft")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let include_deleted = args
        .get("trash")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let input = FindByIdInput::builder(id)
        .depth(depth)
        .locale_ctx(locale_ctx.as_ref())
        .registry(Some(ctx.registry.as_ref()))
        .use_draft(use_draft)
        .include_deleted(include_deleted)
        .build();

    let doc = find_document_by_id(&svc_ctx, &input).map_err(ServiceError::into_anyhow)?;

    match doc {
        Some(d) => Ok(to_string_pretty(&doc_to_json(&d))?),
        None => Ok(to_string_pretty(&NotFoundResponse {
            error: "Document not found",
        })?),
    }
}
