//! Miscellaneous hook tests for crap-cms hook lifecycle.
//!
//! Tests for: `hook_ctx_to_string_map`, `call_row_label`, `call_display_condition`,
//! `run_before_render`, `run_system_hooks`, `run_hooks` (no conn), `run_migration`,
//! `run_job_handler`, and related standalone lifecycle tests.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::used_underscore_binding,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]

use std::path::PathBuf;
use std::sync::Arc;

use crap_cms::config::CrapConfig;
use crap_cms::core::DocumentFields;
use crap_cms::core::HookRef;
use crap_cms::core::JobRun;
use crap_cms::core::{ConditionExpr, ConditionOp, ReqContext};
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks;
use crap_cms::hooks::ConditionContext;
use crap_cms::hooks::lifecycle::{HookContext, HookEvent, HookRunner};
use serde_json::json;

/// A throwaway condition context for `call_display_condition` tests (the
/// condition functions under test only read the form data, not the context).
fn cond_ctx() -> ConditionContext<'static> {
    ConditionContext {
        collection: "posts",
        operation: "update",
        user: None,
        ui_locale: None,
        locale: None,
        options: None,
    }
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
}

fn setup() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    std::sync::Arc<crap_cms::core::Registry>,
    HookRunner,
) {
    let config_dir = fixture_dir();
    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(&config_dir, &config).expect("Failed to init Lua");

    let tmp = tempfile::tempdir().expect("Failed to create temp dir");
    let mut pool_config = CrapConfig::test_default();
    pool_config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &pool_config).expect("Failed to create pool");
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("Failed to sync schema");

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("Failed to create HookRunner");
    (tmp, db_pool, registry, runner)
}

#[allow(dead_code)]
fn create_article(
    pool: &crap_cms::db::DbPool,
    registry: &std::sync::Arc<crap_cms::core::Registry>,
    data: &DocumentFields,
) -> crap_cms::core::Document {
    let def = registry
        .get_collection("articles")
        .expect("articles not found")
        .clone();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let doc = query::create(&tx, "articles", &def, data, None).expect("Create failed");
    tx.commit().expect("Commit");
    doc
}

// ── 6K. HookContext::to_value_map ──────────────────────────────────────────
//
// Unit tests live in `src/hooks/lifecycle/context/hook_context.rs` —
// the integration-test duplicates were removed in alpha.9 when
// `to_string_map` was replaced by `to_value_map`. The unit tests
// already cover all the cases that lived here (typed values flowing
// through, group flattening, non-object groups, nested groups).

// ── 6L. evaluate_condition_table ─────────────────────────────────────────────
//
// Removed in alpha.9. The `evaluate_condition_table` free function was
// replaced by typed [`crap_cms::core::ConditionExpr::evaluate`]; the grammar
// is now exercised directly as unit tests inside `core::condition::tests`.

// ── 6M. call_row_label ───────────────────────────────────────────────────────

#[test]
fn call_row_label_returns_label() {
    let (_tmp, _pool, _registry, runner) = setup();

    let row_data = json!({"label": "My Row"});
    let result = runner.call_row_label("hooks.field_hooks.row_label", &row_data);
    assert_eq!(result, Some("Row: My Row".to_string()));
}

#[test]
fn call_row_label_returns_none_when_no_label() {
    let (_tmp, _pool, _registry, runner) = setup();

    let row_data = json!({"other": "value"});
    let result = runner.call_row_label("hooks.field_hooks.row_label", &row_data);
    assert!(
        result.is_none(),
        "Should return None when label field is missing"
    );
}

#[test]
fn call_row_label_invalid_ref_returns_none() {
    let (_tmp, _pool, _registry, runner) = setup();

    let row_data = json!({"label": "test"});
    let result = runner.call_row_label("hooks.nonexistent.function", &row_data);
    assert!(result.is_none(), "Invalid hook ref should return None");
}

// ── 6N. call_display_condition ───────────────────────────────────────────────

#[test]
fn call_display_condition_bool_true() {
    let (_tmp, _pool, _registry, runner) = setup();

    let data = json!({"status": "published"});
    let result = runner.call_display_condition(
        &HookRef::new("hooks.field_hooks.show_if_published"),
        &data,
        &cond_ctx(),
    );
    assert!(result.is_some());
    match result.unwrap() {
        crap_cms::hooks::lifecycle::DisplayConditionResult::Bool(b) => assert!(b),
        other => panic!("Expected Bool(true), got {other:?}"),
    }
}

#[test]
fn call_display_condition_bool_false() {
    let (_tmp, _pool, _registry, runner) = setup();

    let data = json!({"status": "draft"});
    let result = runner.call_display_condition(
        &HookRef::new("hooks.field_hooks.show_if_published"),
        &data,
        &cond_ctx(),
    );
    assert!(result.is_some());
    match result.unwrap() {
        crap_cms::hooks::lifecycle::DisplayConditionResult::Bool(b) => assert!(!b),
        other => panic!("Expected Bool(false), got {other:?}"),
    }
}

#[test]
fn call_display_condition_table() {
    let (_tmp, _pool, _registry, runner) = setup();

    let data = json!({"status": "published"});
    let result = runner.call_display_condition(
        &HookRef::new("hooks.field_hooks.condition_table"),
        &data,
        &cond_ctx(),
    );
    assert!(result.is_some());
    match result.unwrap() {
        crap_cms::hooks::lifecycle::DisplayConditionResult::Table { condition, visible } => {
            assert!(visible, "status=published should be visible");
            let row = match &condition {
                ConditionExpr::Single(row) => row,
                ConditionExpr::All(_) => panic!("expected single row, got AND"),
            };
            assert_eq!(row.field, "status");
            assert!(matches!(&row.op, ConditionOp::Equals(v) if v == &json!("published")));
        }
        other => panic!("Expected Table, got {other:?}"),
    }
}

#[test]
fn call_display_condition_table_not_visible() {
    let (_tmp, _pool, _registry, runner) = setup();

    let data = json!({"status": "draft"});
    let result = runner.call_display_condition(
        &HookRef::new("hooks.field_hooks.condition_table"),
        &data,
        &cond_ctx(),
    );
    assert!(result.is_some());
    match result.unwrap() {
        crap_cms::hooks::lifecycle::DisplayConditionResult::Table { visible, .. } => {
            assert!(
                !visible,
                "status=draft should not be visible when condition says equals=published"
            );
        }
        other => panic!("Expected Table, got {other:?}"),
    }
}

#[test]
fn call_display_condition_invalid_ref_returns_none() {
    let (_tmp, _pool, _registry, runner) = setup();

    let data = json!({"status": "published"});
    let result = runner.call_display_condition(
        &HookRef::new("hooks.nonexistent.function"),
        &data,
        &cond_ctx(),
    );
    assert!(result.is_none(), "Invalid hook ref should return None");
}

// ── 6O. run_before_render ────────────────────────────────────────────────────

#[test]
fn run_before_render_no_hooks_returns_same() {
    // Default init.lua only registers before_change hooks, not before_render.
    // So this should return the context unchanged.
    let (_tmp, _pool, _registry, runner) = setup();

    let context = json!({"page": "home", "items": [1, 2, 3]});
    let result = runner.run_before_render(context.clone());
    assert_eq!(result, context);
}

// ── 6P. run_system_hooks_with_conn ───────────────────────────────────────────

#[test]
fn run_system_hooks_empty_refs() {
    let (_tmp, pool, _registry, runner) = setup();

    let conn = pool.get().expect("DB connection");
    let result = runner.run_system_hooks_with_conn(&[], &conn);
    assert!(result.is_ok(), "Empty refs should succeed");
}

#[test]
fn run_system_hooks_with_valid_ref() {
    let (_tmp, pool, _registry, runner) = setup();

    let conn = pool.get().expect("DB connection");
    let refs = vec!["hooks.field_hooks.system_init".to_string()];
    let result = runner.run_system_hooks_with_conn(&refs, &conn);
    assert!(result.is_ok(), "System hook with valid ref should succeed");
}

#[test]
fn run_system_hooks_with_invalid_ref_fails() {
    let (_tmp, pool, _registry, runner) = setup();

    let conn = pool.get().expect("DB connection");
    let refs = vec!["hooks.nonexistent.function".to_string()];
    let result = runner.run_system_hooks_with_conn(&refs, &conn);
    assert!(result.is_err(), "System hook with invalid ref should fail");
}

// ── 6Q. run_hooks without conn (no CRUD access) ─────────────────────────────

#[test]
fn run_hooks_no_conn_fires_collection_and_registered() {
    let (_tmp, _pool, registry, runner) = setup();
    let def = registry.get_collection("articles").unwrap().clone();

    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!("Test"));

    let ctx = HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: ReqContext::new(),
        user: None,
        ui_locale: None,
        document_id: None,
        edited_by: None,
    };

    let result = runner
        .run_hooks(&def.hooks, HookEvent::BeforeChange, ctx)
        .expect("run_hooks failed");

    // Collection-level before_change sets _hook_ran
    assert_eq!(
        result.data.get("_hook_ran").and_then(|v| v.as_str()),
        Some("before_change"),
    );
    // Global registered before_change sets _global_hook_ran
    assert_eq!(
        result.data.get("_global_hook_ran").and_then(|v| v.as_str()),
        Some("true"),
    );
}

// ── 6U. run_migration ────────────────────────────────────────────────────────

#[test]
fn run_migration_executes_lua_file() {
    let (_tmp, pool, registry, runner) = setup();

    // Create a temporary migration file
    let migration_dir = tempfile::tempdir().expect("tempdir");
    let migration_path = migration_dir.path().join("001_test.lua");
    std::fs::write(
        &migration_path,
        r#"
        local M = {}
        function M.up()
            -- Create a test article to prove the migration ran
            crap.collections.create("articles", {
                title = "from-migration",
            })
        end
        function M.down()
            -- no-op
        end
        return M
    "#,
    )
    .expect("write migration");

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("tx");

    let result = runner.run_migration(&migration_path, "up", &tx);
    assert!(
        result.is_ok(),
        "Migration should succeed: {:?}",
        result.err()
    );
    tx.commit().unwrap();

    // Verify the migration ran by checking the article was created
    let def = registry.get_collection("articles").unwrap().clone();

    let count =
        crap_cms::db::ops::count_documents(&pool, "articles", &def, &[], None).expect("count");
    assert_eq!(count, 1, "Migration should have created 1 article");
}

#[test]
fn run_migration_invalid_direction_fails() {
    let (_tmp, pool, _registry, runner) = setup();

    let migration_dir = tempfile::tempdir().expect("tempdir");
    let migration_path = migration_dir.path().join("002_test.lua");
    std::fs::write(
        &migration_path,
        r"
        local M = {}
        function M.up() end
        return M
    ",
    )
    .expect("write migration");

    let conn = pool.get().expect("DB connection");
    let result = runner.run_migration(&migration_path, "down", &conn);
    assert!(
        result.is_err(),
        "Migration with missing direction function should fail"
    );
}

// ── 6V. run_job_handler ──────────────────────────────────────────────────────

/// Build a minimal `JobRun` for driving `run_job_handler` directly in tests.
fn job_run(slug: &str, data: &str, attempt: u32, max_attempts: u32) -> JobRun {
    JobRun::builder("test-run-id", slug)
        .data(data)
        .attempt(attempt)
        .max_attempts(max_attempts)
        .build()
}

#[test]
fn run_job_handler_with_valid_function() {
    let (_tmp, pool, _registry, runner) = setup();

    // We can test using eval_lua_with_conn to define a function, then call run_job_handler.
    // But run_job_handler resolves a function ref, so we need to write it to a Lua file.
    // Instead, let's use the field_hooks module which is already loaded.
    // We'll add a simple handler to the field_hooks module.

    // Actually, let's just test that run_job_handler works with a function that's already loaded.
    // The system_init function in field_hooks takes a context table and returns it.
    let result = runner.run_job_handler(
        &HookRef::new("hooks.field_hooks.system_init"),
        &job_run("test-job", r#"{"key": "value"}"#, 1, 3),
        &pool,
    );
    assert!(
        result.is_ok(),
        "Job handler should succeed: {:?}",
        result.err()
    );
}

#[test]
fn run_job_handler_invalid_ref_fails() {
    let (_tmp, pool, _registry, runner) = setup();

    let result = runner.run_job_handler(
        &HookRef::new("hooks.nonexistent.handler"),
        &job_run("test-job", "{}", 1, 3),
        &pool,
    );
    assert!(result.is_err(), "Invalid handler ref should fail");
}

// ── 7A. run_before_render with registered hooks ───────────────────────────────

#[test]
fn before_render_registered_hook_adds_marker() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::write(
        collections_dir.join("articles.lua"),
        r#"crap.collections.define("articles", { fields = { { name = "title", type = "text" } } })"#,
    ).unwrap();
    // Register a before_render hook that adds a marker
    std::fs::write(
        tmp.path().join("init.lua"),
        r#"
        crap.hooks.register("before_render", function(ctx)
            ctx._render_marker = "rendered"
            return ctx
        end)
    "#,
    )
    .unwrap();

    let config = CrapConfig::test_default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new");

    let context = json!({ "page": "edit" });
    let result = runner.run_before_render(context);
    assert_eq!(
        result.get("_render_marker").and_then(|v| v.as_str()),
        Some("rendered"),
        "before_render hook should add _render_marker"
    );
    // Original data preserved
    assert_eq!(result.get("page").and_then(|v| v.as_str()), Some("edit"),);
}

#[test]
fn before_render_hook_returning_nil_preserves_context() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::write(
        collections_dir.join("articles.lua"),
        r#"crap.collections.define("articles", { fields = { { name = "title", type = "text" } } })"#,
    ).unwrap();
    std::fs::write(
        tmp.path().join("init.lua"),
        r#"
        crap.hooks.register("before_render", function(ctx)
            return nil
        end)
    "#,
    )
    .unwrap();

    let config = CrapConfig::test_default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new");

    let context = json!({ "page": "list" });
    let result = runner.run_before_render(context.clone());
    // nil return should keep context unchanged
    assert_eq!(result, context);
}

/// A `before_render` hook that raises a Lua error must not crash the render
/// path. The admin UI falls back to the original context unmodified so the
/// page still renders.
#[test]
fn before_render_hook_error_returns_original_context() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::write(
        collections_dir.join("articles.lua"),
        r#"crap.collections.define("articles", { fields = { { name = "title", type = "text" } } })"#,
    ).unwrap();
    std::fs::write(
        tmp.path().join("init.lua"),
        r#"
        crap.hooks.register("before_render", function(ctx)
            error("intentional before_render failure")
        end)
    "#,
    )
    .unwrap();

    let config = CrapConfig::test_default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new");

    let context = json!({ "page": "edit", "title": "Hello" });
    let result = runner.run_before_render(context.clone());

    // A failing hook must not propagate — the original context is returned
    // unmodified so callers (admin UI render path) can proceed.
    assert_eq!(
        result, context,
        "errors in before_render must fall back to the original context"
    );
}

// ── 7B. run_migration with standalone config dir ──────────────────────────────

#[test]
fn run_migration_up_standalone() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::write(
        collections_dir.join("articles.lua"),
        r#"crap.collections.define("articles", { fields = { { name = "title", type = "text" } } })"#,
    ).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");

    let mut pool_config = CrapConfig::test_default();
    pool_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &pool_config).expect("pool");
    crap_cms::db::migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("HookRunner::new");

    // Write a migration file
    let migration_path = tmp.path().join("migration_test.lua");
    std::fs::write(
        &migration_path,
        r#"
        local M = {}
        function M.up()
            -- Create a document via CRUD
            crap.collections.create("articles", { title = "Migrated Article" })
        end
        function M.down()
            -- No-op
        end
        return M
    "#,
    )
    .unwrap();

    let conn = pool.get().expect("conn");
    runner
        .run_migration(&migration_path, "up", &conn)
        .expect("migration up should succeed");

    // Verify the document was created
    let def = registry.get_collection("articles").expect("articles");
    let docs = crap_cms::db::ops::find_documents(
        &pool,
        "articles",
        def,
        &crap_cms::db::query::FindQuery::default(),
        None,
    )
    .expect("find");
    assert_eq!(docs.len(), 1);
    assert_eq!(
        docs[0].fields.get("title").and_then(|v| v.as_str()),
        Some("Migrated Article")
    );
}

// ── 7C. run_job_handler with standalone Lua files ─────────────────────────────

#[test]
fn run_job_handler_with_return_value() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let collections_dir = tmp.path().join("collections");
    let jobs_dir = tmp.path().join("jobs");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::write(
        collections_dir.join("articles.lua"),
        r#"crap.collections.define("articles", { fields = { { name = "title", type = "text" } } })"#,
    ).unwrap();
    std::fs::write(
        jobs_dir.join("test_job.lua"),
        r"
        local M = {}
        function M.run(ctx)
            return { processed = true, slug = ctx.job.slug, data_value = ctx.data.key }
        end
        return M
    ",
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");

    let mut pool_config = CrapConfig::test_default();
    pool_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &pool_config).expect("pool");
    crap_cms::db::migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new");

    let result = runner
        .run_job_handler(
            &HookRef::new("jobs.test_job.run"),
            &job_run("test-job", r#"{"key": "hello"}"#, 1, 3),
            &pool,
        )
        .expect("run_job_handler failed");

    assert!(result.is_some(), "Job should return a value");
    let result_json: serde_json::Value =
        serde_json::from_str(&result.unwrap()).expect("parse JSON");
    assert_eq!(
        result_json
            .get("processed")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        result_json.get("slug").and_then(|v| v.as_str()),
        Some("test-job")
    );
    assert_eq!(
        result_json.get("data_value").and_then(|v| v.as_str()),
        Some("hello")
    );
}

#[test]
fn run_job_handler_nil_return() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let collections_dir = tmp.path().join("collections");
    let jobs_dir = tmp.path().join("jobs");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::write(
        collections_dir.join("articles.lua"),
        r#"crap.collections.define("articles", { fields = { { name = "title", type = "text" } } })"#,
    ).unwrap();
    std::fs::write(
        jobs_dir.join("void_job.lua"),
        r"
        local M = {}
        function M.run(ctx)
            -- do nothing, return nil
        end
        return M
    ",
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");

    let mut pool_config = CrapConfig::test_default();
    pool_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &pool_config).expect("pool");

    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new");

    let result = runner
        .run_job_handler(
            &HookRef::new("jobs.void_job.run"),
            &job_run("void-job", "{}", 1, 1),
            &pool,
        )
        .expect("run_job_handler failed");

    assert!(result.is_none(), "Job returning nil should give None");
}

// ── 7F. call_row_label and call_display_condition with standalone hooks ───────

#[test]
fn call_row_label_standalone_hook() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(
        collections_dir.join("articles.lua"),
        r#"crap.collections.define("articles", { fields = { { name = "title", type = "text" } } })"#,
    ).unwrap();
    std::fs::write(
        hooks_dir.join("row_label.lua"),
        r#"
        local M = {}
        function M.format(row)
            return "Row: " .. (row.title or "untitled")
        end
        return M
    "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new");

    let row_data = json!({ "title": "Hello" });
    let label = runner.call_row_label("hooks.row_label.format", &row_data);
    assert_eq!(label, Some("Row: Hello".to_string()));
}

#[test]
fn call_display_condition_standalone_bool() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(
        collections_dir.join("articles.lua"),
        r#"crap.collections.define("articles", { fields = { { name = "title", type = "text" } } })"#,
    ).unwrap();
    std::fs::write(
        hooks_dir.join("conditions.lua"),
        r#"
        local M = {}
        function M.show_if_published(data)
            return data.status == "published"
        end
        return M
    "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new");

    let form_data = json!({ "status": "published" });
    let result = runner.call_display_condition(
        &HookRef::new("hooks.conditions.show_if_published"),
        &form_data,
        &cond_ctx(),
    );
    match result {
        Some(crap_cms::hooks::lifecycle::DisplayConditionResult::Bool(b)) => assert!(b),
        other => panic!("Expected Bool(true), got {other:?}"),
    }

    let form_data_draft = json!({ "status": "draft" });
    let result = runner.call_display_condition(
        &HookRef::new("hooks.conditions.show_if_published"),
        &form_data_draft,
        &cond_ctx(),
    );
    match result {
        Some(crap_cms::hooks::lifecycle::DisplayConditionResult::Bool(b)) => assert!(!b),
        other => panic!("Expected Bool(false), got {other:?}"),
    }
}

#[test]
fn call_display_condition_standalone_table() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(
        collections_dir.join("articles.lua"),
        r#"crap.collections.define("articles", { fields = { { name = "title", type = "text" } } })"#,
    ).unwrap();
    std::fs::write(
        hooks_dir.join("conditions.lua"),
        r#"
        local M = {}
        function M.condition_table(data)
            return { field = "status", equals = "published" }
        end
        return M
    "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new");

    let form_data = json!({ "status": "published" });
    let result = runner.call_display_condition(
        &HookRef::new("hooks.conditions.condition_table"),
        &form_data,
        &cond_ctx(),
    );
    match result {
        Some(crap_cms::hooks::lifecycle::DisplayConditionResult::Table { condition, visible }) => {
            assert!(visible, "status=published should match the condition");
            let row = match &condition {
                ConditionExpr::Single(row) => row,
                ConditionExpr::All(_) => panic!("expected single row, got AND"),
            };
            assert_eq!(row.field, "status");
            assert!(matches!(&row.op, ConditionOp::Equals(v) if v == &json!("published")));
        }
        other => panic!("Expected Table result, got {other:?}"),
    }
}
