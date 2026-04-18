//! Execute `update_many` — bulk update multiple documents matching filters.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, json, to_string_pretty};
use tracing::info;

use crate::{
    config::CrapConfig,
    core::{Registry, cache::SharedCache, event::SharedEventTransport},
    db::DbPool,
    hooks::HookRunner,
    mcp::tools::collection::helpers::{extract_data_from_args, parse_where_filters},
    service::{self, ServiceContext, UpdateManyOptions},
};

/// Execute `update_many` — bulk update documents matching a where filter.
#[allow(clippy::too_many_arguments)]
pub(in crate::mcp::tools) fn exec_update_many(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
    config: &CrapConfig,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
) -> Result<String> {
    let def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let filters = parse_where_filters(args);

    let data_obj = args.get("data").cloned().unwrap_or(json!({}));
    let (data, join_data) = extract_data_from_args(&data_obj, &[]);

    let run_hooks = args.get("hooks").and_then(|v| v.as_bool()).unwrap_or(true);

    let draft = args.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);

    let ctx = ServiceContext::collection(slug, def)
        .pool(pool)
        .runner(runner)
        .override_access(true)
        .event_transport(event_transport)
        .cache(cache)
        .build();

    let opts = UpdateManyOptions {
        locale_ctx: None,
        run_hooks,
        draft,
        ui_locale: None,
    };

    let result = service::update_many(&ctx, filters, data, &join_data, &config.locale, &opts)?;

    info!("MCP update_many {}: {} modified", slug, result.modified);

    Ok(to_string_pretty(&json!({
        "modified": result.modified,
        "updated_ids": result.updated_ids,
    }))?)
}
