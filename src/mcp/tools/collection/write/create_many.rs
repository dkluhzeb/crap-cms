//! Execute `create_many` — bulk create multiple documents.

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    db::LocaleContext,
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{doc_to_json, extract_auth_password, extract_data_from_args},
        jobs::{QueuedFields, queue_bulk_tool},
    },
    service::{
        CreateManyItem,
        jobs::bulk_queue::BulkOpKind,
        op::{self, CreateMany, CreateManyArgs, Principal, TargetRef},
    },
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
        .infra
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
        let password = extract_auth_password(def, doc_val, false);

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

    // Honor the write locale exactly like single create — parity across
    // gRPC/MCP/Lua via the wire model.
    let locale = args.get("locale").and_then(Value::as_str);
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;

    if args.get("queue").and_then(Value::as_bool).unwrap_or(false) {
        // The payload persists in the jobs table until execution, so
        // plaintext credentials must never enter it (same rule as gRPC).
        if items.iter().any(|i| i.password.is_some()) {
            anyhow::bail!(
                "queue cannot be combined with per-item passwords — run create_many without queue"
            );
        }

        return queue_bulk_tool(
            ctx,
            slug,
            BulkOpKind::CreateMany,
            QueuedFields {
                locale: locale.map(str::to_string),
                draft,
                hooks: run_hooks,
                events,
                documents: Some(items.into_iter().map(|i| i.data).collect()),
                where_clause: None,
                data: None,
                force_hard_delete: false,
            },
        );
    }

    let op_args = CreateManyArgs::builder(items)
        .run_hooks(run_hooks)
        .draft(draft)
        .locale_ctx(locale_ctx)
        .max_documents(ctx.config.server.bulk_max_documents)
        .events(events)
        .build();

    let result = op::run::<CreateMany>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::collection(slug),
        op_args,
    )
    .map_err(|e| e.into_service_error().into_anyhow_scrubbed())?;

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
