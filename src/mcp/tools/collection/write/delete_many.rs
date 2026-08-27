//! Execute `delete_many` — bulk delete multiple documents matching filters.

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    core::upload,
    mcp::tools::{ToolExecCtx, collection::helpers::parse_where_filters},
    service::{self, DeleteManyOptions, ServiceContext},
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
    let mut def = ctx
        .infra
        .registry
        .collections
        .get(slug)
        .context("Collection not found")?
        .clone();

    let filters = parse_where_filters(args)?;

    let run_hooks = args
        .get("hooks")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    let force_hard_delete = args
        .get("force_hard_delete")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    if force_hard_delete && def.soft_delete {
        def.make_hard_delete();
    }

    let events = args
        .get("events")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let svc_ctx = ServiceContext::collection(slug, &def)
        .pool(&ctx.infra.pool)
        .runner(&ctx.infra.hook_runner)
        .override_access(true)
        .event_transport(ctx.infra.event_transport.clone())
        .invalidation_transport(Some(ctx.infra.invalidation_transport.clone()))
        .emit_events(events)
        .cache(Some(ctx.infra.cache.clone()))
        .build();

    let opts = DeleteManyOptions {
        run_hooks,
        max_documents: ctx.config.server.bulk_max_documents,
        ..Default::default()
    };

    let result = service::delete_many(&svc_ctx, &filters, &ctx.config.locale, &opts)?;

    // Hard-deleted rows leave orphaned upload files — clean them post-commit
    // (mirrors the gRPC/admin surfaces).
    let storage = ctx.infra.storage.as_ref();
    for fields in &result.upload_fields_to_clean {
        upload::delete_upload_files(storage, fields);
    }

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
