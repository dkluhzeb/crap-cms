//! Execute `update_many` — bulk update multiple documents matching filters.

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::{Value, json, to_string_pretty};
use tracing::info;

use crate::{
    db::LocaleContext,
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{extract_data_from_args, parse_where_filters},
    },
    service::{self, ServiceContext, UpdateManyOptions},
};

/// Shape returned to the MCP client for an `update_many` tool call.
#[derive(Serialize)]
struct UpdateManyResponse<'a> {
    modified: i64,
    updated_ids: &'a [String],
}

/// Execute `update_many` — bulk update documents matching a where filter.
pub(in crate::mcp::tools) fn exec_update_many(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let def = ctx
        .registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let filters = parse_where_filters(args)?;

    let data_obj = args.get("data").cloned().unwrap_or(json!({}));
    let data = extract_data_from_args(&data_obj, &[]);

    let run_hooks = args
        .get("hooks")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    let draft = args
        .get("draft")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let events = args
        .get("events")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;

    let svc_ctx = ServiceContext::collection(slug, def)
        .pool(ctx.pool)
        .runner(ctx.runner)
        .override_access(true)
        .event_transport(ctx.event_transport.clone())
        .invalidation_transport(ctx.invalidation_transport.clone())
        .emit_events(events)
        .cache(ctx.cache.clone())
        .build();

    let opts = UpdateManyOptions {
        locale_ctx: locale_ctx.as_ref(),
        run_hooks,
        draft,
        ui_locale: None,
        max_documents: ctx.config.server.bulk_max_documents,
    };

    let result = service::update_many(&svc_ctx, &filters, &data, &ctx.config.locale, &opts)?;

    info!(
        "MCP update_many {}: {} modified [client={}]",
        slug, result.modified, ctx.client_label
    );

    Ok(to_string_pretty(&UpdateManyResponse {
        modified: result.modified,
        updated_ids: &result.updated_ids,
    })?)
}
