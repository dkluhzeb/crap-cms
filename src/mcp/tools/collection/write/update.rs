//! Execute `update` — update an existing document.

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{doc_to_json, extract_data_from_args},
    },
    service::{ServiceContext, WriteInput, update_document},
};

/// Execute `update` — update an existing document.
pub(in crate::mcp::tools) fn exec_update(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let def = ctx
        .registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let password = if def.is_auth_collection() {
        args.get("password")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    } else {
        None
    };

    if let Some(ref pw) = password {
        ctx.config.auth.password_policy.validate(pw)?;
    }

    let data = extract_data_from_args(args, &["id", "password"]);

    let svc_ctx = ServiceContext::collection(slug, def)
        .pool(ctx.pool)
        .runner(ctx.runner)
        .override_access(true)
        .event_transport(ctx.event_transport.clone())
        .cache(ctx.cache.clone())
        .build();

    let (doc, _ctx) = update_document(
        &svc_ctx,
        id,
        WriteInput::builder(data)
            .password(password.as_deref())
            .build(),
    )?;

    info!("MCP update {}: {} [client={}]", slug, id, ctx.client_label);

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
