//! Execute `create` — create a new document.

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    db::LocaleContext,
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{doc_to_json, extract_data_from_args},
    },
    service::{ServiceContext, WriteInput, create_document},
};

/// Execute `create` — create a new document.
pub(in crate::mcp::tools) fn exec_create(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let def = ctx
        .registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let password = if def.is_auth_collection() {
        args.get("password")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string)
    } else {
        None
    };

    if let Some(ref pw) = password {
        ctx.config.auth.password_policy.validate(pw)?;
    }

    // `locale`, `draft`, `events` are reserved top-level keys; `password` is
    // reserved only for auth collections (a non-auth collection may have a
    // legitimate field named `password`, matching Lua).
    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;
    let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(false);
    let events = args.get("events").and_then(Value::as_bool).unwrap_or(true);

    let mut skip_keys: Vec<&str> = vec!["locale", "draft", "events"];
    if def.is_auth_collection() {
        skip_keys.push("password");
    }
    let data = extract_data_from_args(args, &skip_keys, &def.fields)?;

    let svc_ctx = ServiceContext::collection(slug, def)
        .pool(ctx.pool)
        .runner(ctx.runner)
        .override_access(true)
        .event_transport(ctx.event_transport.clone())
        .emit_events(events)
        .cache(ctx.cache.clone())
        .build();

    let (doc, _ctx) = create_document(
        &svc_ctx,
        WriteInput::builder(data)
            .password(password.as_deref())
            .locale_ctx(locale_ctx.as_ref())
            .locale(locale.map(std::string::ToString::to_string))
            .draft(draft)
            .build(),
    )?;

    info!(
        "MCP create {}: {} [client={}]",
        slug, doc.id, ctx.client_label
    );

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
