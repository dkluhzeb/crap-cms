//! Execute `create_many` — bulk create multiple documents.

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{doc_to_json, extract_data_from_args},
    },
    service::{self, CreateManyItem, CreateManyOptions, ServiceContext},
};

/// Shape returned to the MCP client for a `create_many` tool call.
#[derive(Serialize)]
struct CreateManyResponse {
    created: i64,
    documents: Vec<Value>,
}

/// Execute `create_many` — bulk create multiple documents.
pub(in crate::mcp::tools) fn exec_create_many(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let def = ctx
        .registry
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
            let data = extract_data_from_args(doc_val, &["password"]);
            let password = doc_val
                .get("password")
                .and_then(|v| v.as_str())
                .map(std::string::ToString::to_string);

            CreateManyItem { data, password }
        })
        .collect();

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

    let svc_ctx = ServiceContext::collection(slug, def)
        .pool(ctx.pool)
        .runner(ctx.runner)
        .override_access(true)
        .event_transport(ctx.event_transport.clone())
        .emit_events(events)
        .cache(ctx.cache.clone())
        .build();

    let opts = CreateManyOptions {
        run_hooks,
        draft,
        max_documents: ctx.config.server.bulk_max_documents,
    };

    let result = service::create_many(&svc_ctx, &items, &opts)?;

    info!(
        "MCP create_many {}: {} created [client={}]",
        slug, result.created, ctx.client_label
    );

    let response = CreateManyResponse {
        created: result.created,
        documents: result.documents.iter().map(doc_to_json).collect(),
    };

    Ok(to_string_pretty(&response)?)
}
