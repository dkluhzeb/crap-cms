//! Execute `update` — update an existing document.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    config::CrapConfig,
    core::{Registry, event::SharedEventTransport},
    db::DbPool,
    hooks::HookRunner,
    mcp::tools::collection::helpers::{doc_to_json, extract_data_from_args},
    service::{ServiceContext, WriteInput, update_document},
};

/// Execute `update` — update an existing document.
pub(in crate::mcp::tools) fn exec_update(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
    config: &CrapConfig,
    event_transport: Option<SharedEventTransport>,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let def = registry
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
        config.auth.password_policy.validate(pw)?;
    }

    let (data, join_data) = extract_data_from_args(args, &["id", "password"]);

    let ctx = ServiceContext::collection(slug, def)
        .pool(pool)
        .runner(runner)
        .override_access(true)
        .event_transport(event_transport)
        .build();

    let (doc, _ctx) = update_document(
        &ctx,
        id,
        WriteInput::builder(data, &join_data)
            .password(password.as_deref())
            .build(),
    )?;

    info!("MCP update {}: {}", slug, id);

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
