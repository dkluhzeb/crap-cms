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
        jobs::{QueuedFields, queue_bulk_tool},
    },
    service::{
        jobs::bulk_queue::BulkOpKind,
        op::{self, Principal, TargetRef, UpdateMany, UpdateManyArgs},
    },
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
        .infra
        .registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let filters = parse_where_filters(args)?;

    let data_obj = args.get("data").cloned().unwrap_or(json!({}));

    // Reject a password in the patch on auth collections (mirrors gRPC and Lua):
    // the field-driven write path silently ignores it, so a caller would think
    // they rotated a password when nothing happened.
    if def.is_auth_collection()
        && data_obj
            .as_object()
            .is_some_and(|o| o.contains_key("password"))
    {
        anyhow::bail!("Cannot set a password via update_many. Use the single update tool instead.");
    }

    let data = extract_data_from_args(&data_obj, &[], &def.fields)?;

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

    if args.get("queue").and_then(Value::as_bool).unwrap_or(false) {
        return queue_bulk_tool(
            ctx,
            slug,
            BulkOpKind::UpdateMany,
            QueuedFields {
                locale: args
                    .get("locale")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                draft,
                hooks: run_hooks,
                events,
                documents: None,
                // Serialize the SAME object `parse_where_filters` accepted,
                // so a queued run decodes exactly what a synchronous call
                // would have used (a non-object `where` is ignored by both).
                where_clause: args
                    .get("where")
                    .and_then(Value::as_object)
                    .map(|o| Value::Object(o.clone()).to_string()),
                data: Some(data),
                force_hard_delete: false,
            },
        );
    }

    let op_args = UpdateManyArgs::builder(filters, data)
        .locale_ctx(locale_ctx)
        .run_hooks(run_hooks)
        .draft(draft)
        .max_documents(ctx.config.server.bulk_max_documents)
        .events(events)
        .build();

    let result = op::run::<UpdateMany>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::collection(slug),
        op_args,
    )
    .map_err(|e| e.into_service_error().into_anyhow_scrubbed())?;

    info!(
        "MCP update_many {}: {} modified [client={}]",
        slug, result.modified, ctx.client_label
    );

    Ok(to_string_pretty(&UpdateManyResponse {
        modified: result.modified,
        updated_ids: &result.updated_ids,
    })?)
}
