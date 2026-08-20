//! Execute `update_global` — update a global document.

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    db::LocaleContext,
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{doc_to_json, events_flag, extract_data_from_args},
    },
    service::{ServiceContext, WriteInput, update_global_document},
};

/// Execute `update_global` — update a global document.
pub(in crate::mcp::tools) fn exec_update_global(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let def = ctx.registry.globals.get(slug).context("Global not found")?;

    // `locale` is a reserved top-level key — excluded from field data.
    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;

    let events = events_flag(args);
    // Globals support drafts (gated by `has_drafts()` in the service); without
    // reading `draft` here an MCP update of a draft-enabled global always
    // published (and a `draft` key fell into field data and was dropped).
    let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(false);

    let data = extract_data_from_args(args, &["locale", "draft", "events"], &def.fields)?;

    let svc_ctx = ServiceContext::global(slug, def)
        .pool(ctx.pool)
        .runner(ctx.runner)
        .override_access(true)
        .event_transport(ctx.event_transport.clone())
        .emit_events(events)
        .cache(ctx.cache.clone())
        .build();

    let (doc, _ctx) = update_global_document(
        &svc_ctx,
        WriteInput::builder(data)
            .locale_ctx(locale_ctx.as_ref())
            .locale(locale.map(std::string::ToString::to_string))
            .draft(draft)
            .build(),
    )?;

    info!("MCP update global: {} [client={}]", slug, ctx.client_label);

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
