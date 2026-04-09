//! Execute `create` — create a new document.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    config::CrapConfig,
    core::Registry,
    db::DbPool,
    hooks::HookRunner,
    mcp::tools::collection::helpers::{doc_to_json, extract_data_from_args},
    service::{WriteInput, create_document},
};

/// Execute `create` — create a new document.
pub(in crate::mcp::tools) fn exec_create(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
    config: &CrapConfig,
) -> Result<String> {
    let def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let password = if def.is_auth_collection() {
        args.get("password")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };

    if let Some(ref pw) = password {
        config.auth.password_policy.validate(pw)?;
    }

    let (data, join_data) = extract_data_from_args(args, &["password"]);

    let (doc, _ctx) = create_document(
        pool,
        runner,
        slug,
        def,
        WriteInput::builder(data, &join_data)
            .password(password.as_deref())
            .build(),
        None,
    )?;

    info!("MCP create {}: {}", slug, doc.id);

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
