//! Execute `read_global` — read a global document.
//!
//! Codec over [`op::run`] with [`Principal::Override`] and a global
//! [`TargetRef`].

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};

use crate::{
    db::LocaleContext,
    mcp::tools::{ToolExecCtx, collection::helpers::doc_to_json},
    service::{
        ServiceError,
        op::{self, GetGlobal, GetGlobalArgs, Principal, TargetRef},
    },
};

/// Execute `read_global` — read a global document.
pub(in crate::mcp::tools) fn exec_read_global(
    args: &Value,
    slug: &str,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &ctx.config.locale)?;
    let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(false);

    let op_args = GetGlobalArgs::builder()
        .locale_ctx(locale_ctx)
        .include_drafts(draft)
        .build();

    let result = op::run::<GetGlobal>(
        &ctx.infra,
        Principal::Override,
        &TargetRef::global(slug),
        op_args,
    );

    // A global's table and its `default` row exist from startup, so a read
    // that fails is a real backend error on this surface as on every other —
    // never an empty global an agent might then "fill in".
    let doc = result
        .map_err(op::CoreError::into_service_error)
        .map_err(ServiceError::into_anyhow_scrubbed)
        .with_context(|| format!("Failed to read global '{slug}'"))?;

    Ok(to_string_pretty(&doc_to_json(&doc))?)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::exec_read_global;
    use crate::{
        config::CrapConfig,
        core::{Registry, collection::GlobalDefinition},
        db::{DbConnection, migrate, pool},
        hooks::lifecycle::HookRunner,
        mcp::tools::test_helpers::make_exec_ctx,
    };

    /// Regression: a read whose backend error mentioned a missing table came
    /// back as an empty global `{}` on MCP only — a broken schema looked like
    /// an unfilled global an agent might then "fill in". It is an error, as on
    /// every other surface.
    #[test]
    fn a_missing_table_is_an_error_not_an_empty_global() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();

        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
        let shared = Registry::shared();
        shared
            .write()
            .unwrap()
            .register_global(GlobalDefinition::new("settings"));
        let registry = Registry::snapshot(&shared);
        migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();

        db_pool
            .get()
            .unwrap()
            .execute_batch("DROP TABLE _global_settings")
            .unwrap();

        let runner = HookRunner::builder()
            .config_dir(tmp.path())
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .unwrap();
        let ctx = make_exec_ctx(&db_pool, &registry, &runner, &config, tmp.path());

        assert!(exec_read_global(&json!({}), "settings", &ctx).is_err());
    }
}
