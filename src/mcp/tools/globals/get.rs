//! Execute `read_global` — read a global document.

use anyhow::{Context as _, Result};
use serde_json::{Value, json, to_string_pretty};

use crate::{
    db::LocaleContext,
    mcp::tools::{ToolExecCtx, collection::helpers::doc_to_json},
    service::{GetGlobalInput, RunnerReadHooks, ServiceContext, ServiceError, get_global_document},
};

/// Execute `read_global` — read a global document.
pub(in crate::mcp::tools) fn exec_read_global(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let def = ctx.registry.globals.get(slug).context("Global not found")?;
    let conn = ctx.pool.get().context("DB connection")?;
    let hooks = RunnerReadHooks::new(ctx.runner, &conn, None, None).with_override_access();

    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;
    let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(false);

    let svc_ctx = ServiceContext::global(slug, def)
        .pool(ctx.pool)
        .conn(&conn)
        .read_hooks(&hooks)
        .override_access(true)
        .build();

    let input = GetGlobalInput::new(locale_ctx.as_ref(), None).include_drafts(draft);

    match get_global_document(&svc_ctx, &input) {
        Ok(d) => Ok(to_string_pretty(&doc_to_json(&d))?),

        // The global may never have been initialized (backing table missing or
        // the `default` row not inserted yet) — surface that as an empty object
        // rather than an error. Only an internal/backend error can carry that
        // signal; typed access/validation errors must still propagate. The
        // string probe is scoped here because "table does not exist" has no
        // typed variant — it is inherently a backend-specific message.
        Err(ServiceError::Internal(e)) if is_uninitialized_global(&e) => {
            Ok(to_string_pretty(&json!({}))?)
        }

        Err(e) => Err(e.into_anyhow()).context(format!("Failed to read global '{slug}'")),
    }
}

/// Whether an internal error means the global has not been initialized yet:
/// its backing table is missing, or the `default` row has never been inserted.
fn is_uninitialized_global(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        let msg = cause.to_string();
        msg.contains("no such table") || msg.starts_with("Failed to get global")
    })
}
