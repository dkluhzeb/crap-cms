//! Execute `update` — update an existing document.

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};
use tracing::info;

use crate::{
    db::LocaleContext,
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{doc_to_json, extract_data_from_args, reserved_data_keys},
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
            .map(std::string::ToString::to_string)
    } else {
        None
    };

    if let Some(ref pw) = password {
        ctx.config.auth.password_policy.validate(pw)?;
    }

    // `locale`, `draft`, `id`, `events` are reserved top-level keys; `password`
    // is reserved only for auth collections (a non-auth collection may have a
    // legitimate field named `password`, matching Lua).
    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;
    let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(false);
    let events = args.get("events").and_then(Value::as_bool).unwrap_or(true);

    let data = extract_data_from_args(args, &reserved_data_keys(def, true), &def.fields)?;

    let svc_ctx = ServiceContext::collection(slug, def)
        .pool(ctx.pool)
        .runner(ctx.runner)
        .override_access(true)
        .event_transport(ctx.event_transport.clone())
        .invalidation_transport(ctx.invalidation_transport.clone())
        .emit_events(events)
        .cache(ctx.cache.clone())
        .password_policy(Some(&ctx.config.auth.password_policy))
        .build();

    let (doc, _ctx) = update_document(
        &svc_ctx,
        id,
        WriteInput::builder(data)
            .password(password.as_deref())
            .locale_ctx(locale_ctx.as_ref())
            .locale(locale.map(std::string::ToString::to_string))
            .draft(draft)
            .build(),
    )?;

    info!("MCP update {}: {} [client={}]", slug, id, ctx.client_label);

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
