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

    // Auth collections: split each item's `password` off (skipped from the
    // strict field extraction) so the service create chokepoint validates it
    // against the password policy and hashes it — parity with the single create
    // tool, gRPC, and Lua `create_many`. Bulk create is per-item, so seeding
    // auth users with policed passwords in one call is legitimate; only
    // `update_many` (a broadcast that would set one password on many rows)
    // rejects a password. For a non-auth collection `password` is ordinary
    // field data, validated by the strict unknown-field check.
    let is_auth = def.is_auth_collection();
    let skip_keys: &[&str] = if is_auth { &["password"] } else { &[] };

    let mut items: Vec<CreateManyItem> = Vec::with_capacity(documents_arr.len());
    for doc_val in documents_arr {
        let password = if is_auth {
            doc_val
                .get("password")
                .and_then(|v| v.as_str())
                .map(std::string::ToString::to_string)
        } else {
            None
        };

        let data = extract_data_from_args(doc_val, skip_keys, &def.fields)?;
        items.push(CreateManyItem { data, password });
    }

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
        .password_policy(Some(&ctx.config.auth.password_policy))
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
