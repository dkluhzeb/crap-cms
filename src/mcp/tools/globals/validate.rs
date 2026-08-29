//! Execute `validate_global` — check global document data against its
//! field rules without persisting.
//!
//! Codec over [`op::run`] with [`Principal::Override`] — same trusted
//! override as MCP's real `update_global`, so the dry-run predicts what that
//! write would do. Semantics (singleton `default` update against
//! `_global_<slug>`) live in the shared [`ValidateGlobal`] body.

use anyhow::{Context as _, Result};
use serde_json::{Value, json, to_string_pretty};

use crate::{
    db::LocaleContext,
    mcp::tools::{ToolExecCtx, collection::helpers::extract_data_from_args},
    service::op::{self, Principal, TargetRef, ValidateArgs, ValidateGlobal},
};

/// Execute `validate_global` — returns `{ "valid": bool, "errors": { field: message } }`.
pub(in crate::mcp::tools) fn exec_validate_global(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    // The def is needed at decode for the strict unknown-field rejection;
    // the op resolves its own copy from the registry.
    let def = ctx
        .infra
        .registry
        .globals
        .get(slug)
        .context("Global not found")?;

    // `locale` and `draft` are reserved top-level keys — excluded from field data.
    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;
    let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(false);

    let data = extract_data_from_args(args, &["locale", "draft"], &def.fields)?;

    let op_args = ValidateArgs::builder(data)
        .locale_ctx(locale_ctx)
        .draft(draft)
        .build();

    let outcome = op::run::<ValidateGlobal>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::global(slug),
        op_args,
    )
    .map_err(|e| e.into_service_error().into_anyhow())?;

    let (valid, errors) = match outcome {
        None => (true, std::collections::HashMap::new()),
        Some(ve) => (false, ve.to_field_map()),
    };

    Ok(to_string_pretty(
        &json!({ "valid": valid, "errors": errors }),
    )?)
}
