//! Execute `validate_global` — check global document data against its
//! field rules without persisting. Mirrors the admin global-validate
//! endpoint: runs the full before-write pipeline (field coercion,
//! validators, `before_validate` hooks) and reports per-field errors.

use anyhow::{Context as _, Result};
use serde_json::{Value, json, to_string_pretty};

use crate::{
    db::{LocaleContext, query::helpers::global_table},
    mcp::tools::{ToolExecCtx, collection::helpers::extract_data_from_args},
    service::{RunnerWriteHooks, ServiceError, ValidateContext, WriteInput, validate_document},
};

/// Execute `validate_global` — returns `{ "valid": bool, "errors": { field: message } }`.
pub(in crate::mcp::tools) fn exec_validate_global(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let def = ctx.registry.globals.get(slug).context("Global not found")?;

    let conn = ctx.pool.get().context("DB connection")?;

    // `locale` and `draft` are reserved top-level keys — excluded from field data.
    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;
    let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(false) && def.has_drafts();

    let data = extract_data_from_args(args, &["locale", "draft"]);

    let write_hooks = RunnerWriteHooks::new(ctx.runner);

    // Globals are a singleton row keyed `default` in the `_global_<slug>`
    // table — validate as an update that excludes that row from unique checks.
    let table = global_table(slug);
    let validate_ctx = ValidateContext {
        slug,
        table_name: &table,
        fields: &def.fields,
        hooks: &def.hooks,
        operation: "update",
        exclude_id: Some("default"),
        soft_delete: false,
        required_locales: None,
    };

    let input = WriteInput::builder(data)
        .locale_ctx(locale_ctx.as_ref())
        .draft(draft)
        .build();

    let result = match validate_document(&conn, &write_hooks, &validate_ctx, input, None) {
        Ok(()) => json!({ "valid": true, "errors": {} }),
        Err(ServiceError::Validation(ve)) => {
            json!({ "valid": false, "errors": ve.to_field_map() })
        }
        Err(e) => return Err(e.into_anyhow()),
    };

    Ok(to_string_pretty(&result)?)
}
