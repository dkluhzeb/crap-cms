//! Execute `update` — update an existing document.
//!
//! Codec over [`op::run`] with [`Principal::Override`].

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
    service::op::{self, Principal, TargetRef, Update, UpdateArgs},
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
        .infra
        .registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let password = extract_auth_password(def, args, true);

    if let Some(ref pw) = password {
        ctx.config.auth.password_policy.validate(pw)?;
    }

    // `locale`, `draft`, `id`, `events` are reserved top-level keys; `password`
    // is reserved only for auth collections (a non-auth collection may have a
    // legitimate field named `password`, matching Lua).
    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;
    let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(false);
    let events = events_flag(args);

    let data = extract_data_from_args(args, &reserved_data_keys(def, true), &def.fields)?;

    let op_args = UpdateArgs::builder(id, data)
        .password(password)
        .locale_ctx(locale_ctx)
        .draft(draft)
        .events(events)
        .build();

    let (doc, _req_context) = op::run::<Update>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::collection(slug),
        op_args,
    )
    .map_err(|e| e.into_service_error().into_anyhow_scrubbed())?;

    info!("MCP update {}: {} [client={}]", slug, id, ctx.client_label);

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}
