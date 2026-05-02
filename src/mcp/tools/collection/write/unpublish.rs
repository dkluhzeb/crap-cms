//! Execute `unpublish` — unpublish a versioned document.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    config::CrapConfig,
    core::{Registry, cache::SharedCache, event::SharedEventTransport},
    db::DbPool,
    hooks::HookRunner,
    mcp::tools::collection::helpers::doc_to_json,
    service::{ServiceContext, unpublish_document},
};

/// Parameters for `exec_unpublish`. All fields required, single call
/// site (`mcp::tools::dispatch::ToolOp::Unpublish`) — plain struct
/// literal per CLAUDE.md (builder reserved for >2-field structs that
/// are constructed in multiple places).
pub(in crate::mcp::tools) struct UnpublishParams<'a> {
    pub args: &'a Value,
    pub slug: &'a str,
    pub registry: &'a Arc<Registry>,
    pub pool: &'a DbPool,
    pub runner: &'a HookRunner,
    pub config: &'a CrapConfig,
    pub event_transport: Option<SharedEventTransport>,
    pub cache: Option<SharedCache>,
}

/// Execute `unpublish` — set a document to draft status.
pub(in crate::mcp::tools) fn exec_unpublish(p: UnpublishParams<'_>) -> Result<String> {
    let id = p
        .args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let def = p
        .registry
        .collections
        .get(p.slug)
        .context("Collection not found")?;

    let ctx = ServiceContext::collection(p.slug, def)
        .pool(p.pool)
        .runner(p.runner)
        .override_access(true)
        .event_transport(p.event_transport)
        .cache(p.cache)
        // Required so the raw read inside `unpublish_document_core` builds
        // a default `LocaleContext` for collections with localized fields.
        // Without this, the SELECT references bare column names that
        // don't exist when locales are enabled.
        .locale_config(Some(&p.config.locale))
        .build();

    let doc = unpublish_document(&ctx, id)?;

    info!("MCP unpublish {}: {}", p.slug, id);

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
