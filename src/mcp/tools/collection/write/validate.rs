//! Execute `validate` — check document data against collection rules
//! without persisting. Mirrors the gRPC `Validate` RPC: runs the full
//! before-write pipeline (field coercion, validators, unique checks,
//! `before_validate` hooks) and reports per-field errors instead of
//! writing a row.

use anyhow::{Context as _, Result};
use serde_json::{Value, json, to_string_pretty};

use crate::{
    db::LocaleContext,
    mcp::tools::{
        ToolExecCtx,
        collection::helpers::{extract_data_from_args, reserved_data_keys},
    },
    service::{
        RunnerWriteHooks, ServiceError, ValidateContext, WriteInput, validate_document,
        validate_outcome,
    },
};

/// Execute `validate` — returns `{ "valid": bool, "errors": { field: message } }`.
pub(in crate::mcp::tools) fn exec_validate(
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

    let conn = ctx.infra.pool.get().context("DB connection")?;

    // `id`, `locale`, `draft`, and `password` are reserved top-level keys —
    // excluded from the document field data (mirrors the create/update tools).
    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;
    let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(false);

    let exclude_id = args
        .get("id")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
    let operation = if exclude_id.is_some() {
        "update"
    } else {
        "create"
    };

    // Reserved keys now include `events` (was omitted here, so a validate call
    // passing `events` errored as an unknown field where create/update strip it).
    let data = extract_data_from_args(args, &reserved_data_keys(def, true), &def.fields)?;

    // Attach the connection so field-level write-access denials are actually
    // evaluated (a connless hooks object skips them, diverging from the real
    // write path).
    let write_hooks = RunnerWriteHooks::new(&ctx.infra.hook_runner).with_conn(&conn);

    let validate_ctx = ValidateContext {
        slug,
        table_name: slug,
        fields: &def.fields,
        hooks: &def.hooks,
        operation,
        exclude_id: exclude_id.as_deref(),
        soft_delete: def.soft_delete,
        supports_drafts: def.has_drafts(),
        required_locales: def.required_locales.as_ref(),
    };

    let input = WriteInput::builder(data)
        .locale_ctx(locale_ctx.as_ref())
        .draft(draft)
        .build();

    let (valid, errors) = validate_outcome(validate_document(
        &conn,
        &write_hooks,
        &validate_ctx,
        input,
        None,
    ))
    .map_err(ServiceError::into_anyhow)?;

    Ok(to_string_pretty(
        &json!({ "valid": valid, "errors": errors }),
    )?)
}
