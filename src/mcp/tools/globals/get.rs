//! Execute `read_global` — read a global document.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{json, to_string_pretty};

use crate::{
    core::Registry,
    db::DbPool,
    hooks::HookRunner,
    mcp::tools::collection::helpers::doc_to_json,
    service::{GetGlobalInput, RunnerReadHooks, ServiceContext, get_global_document},
};

/// Execute `read_global` — read a global document.
pub(in crate::mcp::tools) fn exec_read_global(
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
) -> Result<String> {
    let def = registry.globals.get(slug).context("Global not found")?;
    let conn = pool.get().context("DB connection")?;
    let hooks = RunnerReadHooks::new(runner, &conn);

    let ctx = ServiceContext::global(slug, def)
        .pool(pool)
        .conn(&conn)
        .read_hooks(&hooks)
        .override_access(true)
        .build();

    let input = GetGlobalInput::new(None, None);

    match get_global_document(&ctx, &input).map_err(|e| e.into_anyhow()) {
        Ok(d) => Ok(to_string_pretty(&doc_to_json(&d))?),
        Err(e) => {
            // The global row may not exist yet (table missing or default row not inserted).
            let is_missing = e.chain().any(|cause| {
                let msg = cause.to_string();
                msg.contains("no such table") || msg.starts_with("Failed to get global")
            });

            if is_missing {
                Ok(json!({}).to_string())
            } else {
                Err(e).context(format!("Failed to read global '{}'", slug))
            }
        }
    }
}
