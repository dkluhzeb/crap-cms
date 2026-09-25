//! Execute `read_global` — read a global document.
//!
//! Codec over [`op::run`] with [`Principal::Override`] and a global
//! [`TargetRef`].

use anyhow::{Context as _, Result};
use serde_json::{Value, to_string_pretty};

use crate::{
    db::{LocaleContext, query},
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

    // Resolved as a collection read resolves it: absent (or outside i32) is
    // the configured default, then floored at 0 and capped at max_depth.
    let requested = args
        .get("depth")
        .and_then(Value::as_i64)
        .and_then(|d| i32::try_from(d).ok());
    let depth = query::clamp_depth(
        requested,
        ctx.config.depth.default_depth,
        ctx.config.depth.max_depth,
    );

    let op_args = GetGlobalArgs::builder()
        .locale_ctx(locale_ctx)
        .include_drafts(draft)
        .depth(depth)
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

    use serde_json::{Value, from_str, json};

    use super::exec_read_global;
    use crate::{
        config::CrapConfig,
        core::{
            CollectionDefinition, FieldDefinition, FieldType, Registry, RelationshipConfig,
            collection::GlobalDefinition,
        },
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

    /// Regression: `read_global` had no `depth` and never populated a
    /// global's relationships. It populates like `find_by_id` — the
    /// configured default when unset — and `depth = 0` returns the id.
    #[test]
    fn read_global_populates_relationships_to_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();

        let mut tags = CollectionDefinition::new("tags");
        tags.fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];
        let mut settings = GlobalDefinition::new("settings");
        settings.fields = vec![
            FieldDefinition::builder("featured", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", false))
                .build(),
        ];

        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
        let shared = Registry::shared();
        {
            let mut reg = shared.write().unwrap();
            reg.register_collection(tags);
            reg.register_global(settings);
        }
        let registry = Registry::snapshot(&shared);
        migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();

        db_pool
            .get()
            .unwrap()
            .execute_batch(
                "INSERT INTO tags (id, name) VALUES ('t1', 'Rust');
                 UPDATE _global_settings SET featured = 't1' WHERE id = 'default';",
            )
            .unwrap();

        let runner = HookRunner::builder()
            .config_dir(tmp.path())
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .unwrap();
        let ctx = make_exec_ctx(&db_pool, &registry, &runner, &config, tmp.path());

        let read = |args| -> Value {
            from_str(&exec_read_global(&args, "settings", &ctx).unwrap()).unwrap()
        };

        for args in [json!({}), json!({ "depth": 1 })] {
            let doc = read(args);
            assert_eq!(doc["featured"]["name"], "Rust", "{doc}");
            assert_eq!(doc["featured"]["collection"], "tags", "{doc}");
        }

        assert_eq!(read(json!({ "depth": 0 }))["featured"], "t1");
    }
}
