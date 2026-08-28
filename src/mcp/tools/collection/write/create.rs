//! Execute `create` — create a new document.
//!
//! Codec over [`op::run`] with [`Principal::Override`]. Dispatching through
//! the operation core also fixes the piecemeal context this tool used to
//! build (it silently omitted the invalidation transport, email context, and
//! locale config).

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    db::LocaleContext,
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{
            doc_to_json, events_flag, extract_auth_password, extract_data_from_args,
            reserved_data_keys,
        },
    },
    service::op::{self, Create, CreateArgs, Principal, TargetRef},
};

/// Execute `create` — create a new document.
pub(in crate::mcp::tools) fn exec_create(
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

    let password = extract_auth_password(def, args, false);

    if let Some(ref pw) = password {
        ctx.config.auth.password_policy.validate(pw)?;
    }

    // `locale`, `draft`, `events` are reserved top-level keys; `password` is
    // reserved only for auth collections (a non-auth collection may have a
    // legitimate field named `password`, matching Lua).
    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;
    let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(false);
    let events = events_flag(args);

    let data = extract_data_from_args(args, &reserved_data_keys(def, false), &def.fields)?;

    let op_args = CreateArgs::builder(data)
        .password(password)
        .locale_ctx(locale_ctx)
        .draft(draft)
        .events(events)
        .build();

    let (doc, _req_context) = op::run::<Create>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::collection(slug),
        op_args,
    )
    .map_err(|e| e.into_service_error().into_anyhow())?;

    info!(
        "MCP create {}: {} [client={}]",
        slug, doc.id, ctx.client_label
    );

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
