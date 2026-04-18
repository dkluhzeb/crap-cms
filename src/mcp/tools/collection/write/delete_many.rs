//! Execute `delete_many` — bulk delete multiple documents matching filters.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, json, to_string_pretty};
use tracing::info;

use crate::{
    config::CrapConfig,
    core::{
        Registry,
        cache::SharedCache,
        event::{SharedEventTransport, SharedInvalidationTransport},
    },
    db::DbPool,
    hooks::HookRunner,
    mcp::tools::collection::helpers::parse_where_filters,
    service::{self, DeleteManyOptions, ServiceContext},
};

/// Execute `delete_many` — bulk delete documents matching a where filter.
#[allow(clippy::too_many_arguments)]
pub(in crate::mcp::tools) fn exec_delete_many(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
    config: &CrapConfig,
    event_transport: Option<SharedEventTransport>,
    invalidation_transport: Option<SharedInvalidationTransport>,
    cache: Option<SharedCache>,
) -> Result<String> {
    let mut def = registry
        .collections
        .get(slug)
        .context("Collection not found")?
        .clone();

    let filters = parse_where_filters(args);

    let run_hooks = args.get("hooks").and_then(|v| v.as_bool()).unwrap_or(true);

    let force_hard_delete = args
        .get("force_hard_delete")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if force_hard_delete && def.soft_delete {
        def.soft_delete = false;
    }

    let ctx = ServiceContext::collection(slug, &def)
        .pool(pool)
        .runner(runner)
        .override_access(true)
        .event_transport(event_transport)
        .invalidation_transport(invalidation_transport)
        .cache(cache)
        .build();

    let opts = DeleteManyOptions {
        run_hooks,
        ..Default::default()
    };

    let result = service::delete_many(&ctx, filters, &config.locale, &opts)?;

    info!(
        "MCP delete_many {}: {} hard, {} soft, {} skipped",
        slug, result.hard_deleted, result.soft_deleted, result.skipped
    );

    Ok(to_string_pretty(&json!({
        "hard_deleted": result.hard_deleted,
        "soft_deleted": result.soft_deleted,
        "skipped": result.skipped,
        "deleted_ids": result.deleted_ids,
    }))?)
}
