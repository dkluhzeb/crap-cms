//! Execute `validate` — check document data against collection rules
//! without persisting.
//!
//! Codec over [`op::run`] with [`Principal::Override`]: the dry-run now runs
//! with the same trusted override as MCP's real `create`/`update`, so its
//! outcome predicts exactly what that write would do. (It previously ran as
//! an anonymous user WITHOUT override, so field-level access rules could
//! strip fields in the dry-run that the actual override write kept.)

use anyhow::Result;
use serde_json::{Value, json, to_string_pretty};

use crate::{
    db::LocaleContext,
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{extract_data_from_args, reserved_data_keys},
    },
    service::op::{self, Principal, TargetRef, Validate, ValidateArgs},
};

/// Execute `validate` — returns `{ "valid": bool, "errors": { field: message } }`.
pub(in crate::mcp::tools) fn exec_validate(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    // `id`, `locale`, `draft`, and `password` are reserved top-level keys —
    // excluded from the document field data (mirrors the create/update tools).
    // The def is needed at decode for the reserved-key set and the strict
    // unknown-field rejection; the op resolves its own copy from the registry.
    let def = ctx
        .infra
        .registry
        .collections
        .get(slug)
        .ok_or_else(|| anyhow::anyhow!("Collection not found"))?;

    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;
    let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(false);
    let exclude_id = args
        .get("id")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);

    let data = extract_data_from_args(args, &reserved_data_keys(def, true), &def.fields)?;

    let op_args = ValidateArgs::builder(data)
        .locale_ctx(locale_ctx)
        .exclude_id(exclude_id)
        .draft(draft)
        .build();

    let outcome = op::run::<Validate>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::collection(slug),
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
