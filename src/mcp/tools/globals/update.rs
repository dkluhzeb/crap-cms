//! Execute `update_global` — update a global document.

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    db::LocaleContext,
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{
            EXPECTED_REVISION_KEY, doc_to_json, events_flag, expected_revision_arg,
            extract_data_from_args,
        },
    },
    service::op::{self, Principal, TargetRef, UpdateGlobal, UpdateGlobalArgs},
};

/// Execute `update_global` — update a global document.
pub(in crate::mcp::tools) fn exec_update_global(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let def = ctx
        .infra
        .registry
        .globals
        .get(slug)
        .context("Global not found")?;

    // `locale`, `draft`, `events` and `expected_revision` are reserved
    // top-level keys — excluded from field data.
    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;

    let events = events_flag(args);
    // Globals support drafts (gated by `has_drafts()` in the service); without
    // reading `draft` here an MCP update of a draft-enabled global always
    // published (and a `draft` key fell into field data and was dropped).
    let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(false);

    let expected_revision = expected_revision_arg(args)?;

    let data = extract_data_from_args(
        args,
        &["locale", "draft", "events", EXPECTED_REVISION_KEY],
        &def.fields,
    )?;

    let op_args = UpdateGlobalArgs::builder(data)
        .locale_ctx(locale_ctx)
        .draft(draft)
        .events(events)
        .expected_revision(expected_revision)
        .build();

    let (doc, _req_context) = op::run::<UpdateGlobal>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::global(slug),
        op_args,
    )
    .map_err(|e| e.into_service_error().into_anyhow_scrubbed())?;

    info!("MCP update global: {} [client={}]", slug, ctx.client_label);

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
