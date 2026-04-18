//! Execute `create_many` — bulk create multiple documents.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, json, to_string_pretty};
use tracing::info;

use crate::{
    core::{Registry, cache::SharedCache, event::SharedEventTransport},
    db::DbPool,
    hooks::HookRunner,
    mcp::tools::collection::helpers::{doc_to_json, extract_data_from_args},
    service::{self, CreateManyItem, CreateManyOptions, ServiceContext},
};

/// Execute `create_many` — bulk create multiple documents.
pub(in crate::mcp::tools) fn exec_create_many(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
) -> Result<String> {
    let def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let documents_arr = args
        .get("documents")
        .and_then(|v| v.as_array())
        .context("'documents' must be an array")?;

    let items: Vec<CreateManyItem> = documents_arr
        .iter()
        .map(|doc_val| {
            let (data, join_data) = extract_data_from_args(doc_val, &["password"]);
            let password = doc_val
                .get("password")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            CreateManyItem {
                data,
                join_data,
                password,
            }
        })
        .collect();

    let run_hooks = args.get("hooks").and_then(|v| v.as_bool()).unwrap_or(true);

    let draft = args.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);

    let ctx = ServiceContext::collection(slug, def)
        .pool(pool)
        .runner(runner)
        .override_access(true)
        .event_transport(event_transport)
        .cache(cache)
        .build();

    let opts = CreateManyOptions { run_hooks, draft };

    let result = service::create_many(&ctx, items, &opts)?;

    info!("MCP create_many {}: {} created", slug, result.created);

    let docs_json: Vec<Value> = result.documents.iter().map(doc_to_json).collect();

    Ok(to_string_pretty(&json!({
        "created": result.created,
        "documents": docs_json,
    }))?)
}
