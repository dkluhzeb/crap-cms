//! Execute `delete_many` — bulk delete multiple documents matching filters.

use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    mcp::tools::{ToolExecCtx, collection::helpers::parse_where_filters},
    service::op::{self, DeleteMany, DeleteManyArgs, Principal, TargetRef},
};

/// Shape returned to the MCP client for a `delete_many` tool call.
///
/// The internal `DeleteManyResult` also carries `upload_fields_to_clean`
/// (per-row upload-field maps used to delete files post-commit); the
/// files are cleaned via the storage backend below, but the raw maps are
/// deliberately excluded from the wire response.
#[derive(Serialize)]
struct DeleteManyResponse<'a> {
    hard_deleted: i64,
    soft_deleted: i64,
    skipped: i64,
    deleted_ids: &'a [String],
}

/// Execute `delete_many` — bulk delete documents matching a where filter.
pub(in crate::mcp::tools) fn exec_delete_many(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let filters = parse_where_filters(args)?;

    let run_hooks = args
        .get("hooks")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    let force_hard_delete = args
        .get("force_hard_delete")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let events = args
        .get("events")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    // The operation body handles `force_hard_delete` (definition adjustment)
    // and the post-commit upload-file cleanup — identical on every surface.
    let op_args = DeleteManyArgs::builder(filters)
        .run_hooks(run_hooks)
        .force_hard_delete(force_hard_delete)
        .max_documents(ctx.config.server.bulk_max_documents)
        .events(events)
        .build();

    let result = op::run::<DeleteMany>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::collection(slug),
        op_args,
    )
    .map_err(|e| e.into_service_error().into_anyhow())?;

    info!(
        "MCP delete_many {}: {} hard, {} soft, {} skipped [client={}]",
        slug, result.hard_deleted, result.soft_deleted, result.skipped, ctx.client_label
    );

    Ok(to_string_pretty(&DeleteManyResponse {
        hard_deleted: result.hard_deleted,
        soft_deleted: result.soft_deleted,
        skipped: result.skipped,
        deleted_ids: &result.deleted_ids,
    })?)
}
