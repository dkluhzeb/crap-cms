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

    let depth = args
        .get("depth")
        .and_then(|v| v.as_i64())
        .unwrap_or(ctx.config.depth.default_depth as i64) as i32;
    let depth = depth.min(ctx.config.depth.max_depth);

    let hooks = RunnerReadHooks::new(ctx.runner, &conn);
    let svc_ctx = ServiceContext::collection(slug, def)
        .pool(ctx.pool)
        .conn(&conn)
        .read_hooks(&hooks)
        .override_access(true)
        .build();

    let input = FindByIdInput::builder(id)
        .depth(depth)
        .locale_ctx(locale_ctx.as_ref())
        .registry(Some(ctx.registry.as_ref()))
        .build();

    let doc = find_document_by_id(&svc_ctx, &input).map_err(ServiceError::into_anyhow)?;

    match doc {
        Some(d) => Ok(to_string_pretty(&doc_to_json(&d))?),
        None => Ok(to_string_pretty(&NotFoundResponse {
            error: "Document not found",
        })?),
    }
}
