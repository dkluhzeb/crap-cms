//! Miscellaneous hook tests for crap-cms hook lifecycle.
//!
//! Tests for: hook_ctx_to_string_map, evaluate_condition_table,
//! call_row_label, call_display_condition, run_before_render,
//! run_system_hooks, run_hooks (no conn), run_migration, run_job_handler,
//! and related standalone lifecycle tests.

use std::collections::HashMap;
use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::{HookContext, HookEvent, HookRunner};
use serde_json::json;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
}

fn setup() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    crap_cms::core::SharedRegistry,
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
        .registry(registry.clone())
        .config(&config)
        .build()
        .expect("Failed to create HookRunner");
    (tmp, db_pool, registry, runner)
}

#[allow(dead_code)]
fn create_article(
    pool: &crap_cms::db::DbPool,
    registry: &crap_cms::core::SharedRegistry,
    data: &HashMap<String, String>,
) -> crap_cms::core::Document {
    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("articles")
        .expect("articles not found")
        .clone();
    drop(reg);

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let doc = query::create(&tx, "articles", &def, data, None).expect("Create failed");
    tx.commit().expect("Commit");
    doc
}

fn make_field(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, field_type).build()
}

// ── 6K. HookContext::to_string_map ──────────────────────────────────────────

#[test]
fn to_string_map_basic() {
    let mut data = HashMap::new();
    data.insert("title".to_string(), json!("Hello"));
    data.insert("count".to_string(), json!(42));

    let ctx = HookContext::builder("test", "create").data(data).build();

    let fields = vec![
        make_field("title", FieldType::Text),
        make_field("count", FieldType::Number),
    ];

    let map = ctx.to_string_map(&fields);
    assert_eq!(map.get("title").unwrap(), "Hello");
    assert_eq!(map.get("count").unwrap(), "42");
}

#[test]
fn to_string_map_flattens_groups() {
    let mut data = HashMap::new();
    let mut seo = serde_json::Map::new();
    seo.insert("meta_title".to_string(), json!("SEO Title"));
    seo.insert("meta_desc".to_string(), json!("Description"));
    data.insert("seo".to_string(), serde_json::Value::Object(seo));
    data.insert("title".to_string(), json!("Normal Title"));

    let ctx = HookContext::builder("test", "create").data(data).build();

    let fields = vec![
        make_field("title", FieldType::Text),
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                make_field("meta_title", FieldType::Text),
                make_field("meta_desc", FieldType::Text),
            ])
            .build(),
    ];

    let map = ctx.to_string_map(&fields);
    assert_eq!(map.get("title").unwrap(), "Normal Title");
    assert_eq!(map.get("seo__meta_title").unwrap(), "SEO Title");
    assert_eq!(map.get("seo__meta_desc").unwrap(), "Description");
    assert!(
        !map.contains_key("seo"),
        "Group key itself should not be in the map"
    );
}

#[test]
fn to_string_map_group_as_string_falls_through() {
    // When group value is already a string (e.g. from form data), it should be kept as-is
    let mut data = HashMap::new();
    data.insert("seo".to_string(), json!("already-a-string"));

    let ctx = HookContext::builder("test", "create").data(data).build();

    let fields = vec![
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![make_field("meta_title", FieldType::Text)])
            .build(),
    ];

    let map = ctx.to_string_map(&fields);
    // When not an object, falls through to string insertion
    assert_eq!(map.get("seo").unwrap(), "already-a-string");
}

// ── 6L. evaluate_condition_table ─────────────────────────────────────────────

#[test]
fn evaluate_condition_equals() {
    let data = json!({"status": "published"});
    let condition = json!({"field": "status", "equals": "published"});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &condition, &data
    ));

    let condition = json!({"field": "status", "equals": "draft"});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(
        &condition, &data
    ));
}

#[test]
fn evaluate_condition_not_equals() {
    let data = json!({"status": "published"});
    let condition = json!({"field": "status", "not_equals": "draft"});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &condition, &data
    ));

    let condition = json!({"field": "status", "not_equals": "published"});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(
        &condition, &data
    ));
}

#[test]
fn evaluate_condition_in() {
    let data = json!({"status": "published"});
    let condition = json!({"field": "status", "in": ["published", "draft"]});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &condition, &data
    ));

    let condition = json!({"field": "status", "in": ["archived", "deleted"]});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(
        &condition, &data
    ));
}

#[test]
fn evaluate_condition_not_in() {
    let data = json!({"status": "published"});
    let condition = json!({"field": "status", "not_in": ["draft", "archived"]});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &condition, &data
    ));

    let condition = json!({"field": "status", "not_in": ["published", "draft"]});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(
        &condition, &data
    ));
}

#[test]
fn evaluate_condition_is_truthy() {
    let data = json!({"active": true, "name": "test", "empty": "", "flag": false, "nothing": null});

    let cond = json!({"field": "active", "is_truthy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));

    let cond = json!({"field": "name", "is_truthy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));

    let cond = json!({"field": "empty", "is_truthy": true});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));

    let cond = json!({"field": "flag", "is_truthy": true});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));

    let cond = json!({"field": "nothing", "is_truthy": true});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));
}

#[test]
fn evaluate_condition_is_falsy() {
    let data = json!({"active": false, "name": ""});

    let cond = json!({"field": "active", "is_falsy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));

    let cond = json!({"field": "name", "is_falsy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));

    let cond = json!({"field": "missing", "is_falsy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));
}

#[test]
fn evaluate_condition_array_means_and() {
    let data = json!({"status": "published", "role": "admin"});

    // All conditions true => true
    let conditions = json!([
        {"field": "status", "equals": "published"},
        {"field": "role", "equals": "admin"}
    ]);
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &conditions,
        &data
    ));

    // One false => false
    let conditions = json!([
        {"field": "status", "equals": "published"},
        {"field": "role", "equals": "editor"}
    ]);
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(
        &conditions,
        &data
    ));
}

#[test]
fn evaluate_condition_unknown_operator_shows() {
    let data = json!({"x": 1});
    let cond = json!({"field": "x", "unknown_op": "whatever"});
    // Unknown operator defaults to true (show field)
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));
}

#[test]
fn evaluate_condition_non_object_non_array() {
    let data = json!({"x": 1});
    // Non-object, non-array condition defaults to true
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &json!("string"),
        &data
    ));
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &json!(42),
        &data
    ));
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &json!(true),
        &data
    ));
}

#[test]
fn evaluate_condition_is_truthy_with_numbers_arrays_objects() {
    let data = json!({
        "count": 42,
        "zero": 0,
        "items": [1, 2],
        "meta": {"key": "val"},
        "empty_arr": [],
        "empty_obj": {}
    });

    // Non-zero numbers are truthy
    let cond = json!({"field": "count", "is_truthy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));

    // Zero is falsy
    let cond = json!({"field": "zero", "is_truthy": true});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));

    // Non-empty arrays are truthy
    let cond = json!({"field": "items", "is_truthy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));

    // Non-empty objects are truthy
    let cond = json!({"field": "meta", "is_truthy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));

    // Empty arrays are falsy
    let cond = json!({"field": "empty_arr", "is_truthy": true});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));

    // Empty objects are falsy
    let cond = json!({"field": "empty_obj", "is_truthy": true});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(
        &cond, &data
    ));
}

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
    let result = runner.call_display_condition("hooks.field_hooks.show_if_published", &data);
    assert!(result.is_some());
    match result.unwrap() {
        crap_cms::hooks::lifecycle::DisplayConditionResult::Bool(b) => assert!(b),
        other => panic!("Expected Bool(true), got {:?}", other),
    }
}

#[test]
fn call_display_condition_bool_false() {
    let (_tmp, _pool, _registry, runner) = setup();

    let data = json!({"status": "draft"});
    let result = runner.call_display_condition("hooks.field_hooks.show_if_published", &data);
    assert!(result.is_some());
    match result.unwrap() {
        crap_cms::hooks::lifecycle::DisplayConditionResult::Bool(b) => assert!(!b),
        other => panic!("Expected Bool(false), got {:?}", other),
    }
}

#[test]
fn call_display_condition_table() {
    let (_tmp, _pool, _registry, runner) = setup();

    let data = json!({"status": "published"});
    let result = runner.call_display_condition("hooks.field_hooks.condition_table", &data);
    assert!(result.is_some());
    match result.unwrap() {
        crap_cms::hooks::lifecycle::DisplayConditionResult::Table { condition, visible } => {
            assert!(visible, "status=published should be visible");
            assert_eq!(
                condition.get("field").and_then(|v| v.as_str()),
                Some("status")
            );
            assert_eq!(
                condition.get("equals").and_then(|v| v.as_str()),
                Some("published")
            );
        }
        other => panic!("Expected Table, got {:?}", other),
    }
}

#[test]
fn call_display_condition_table_not_visible() {
    let (_tmp, _pool, _registry, runner) = setup();

    let data = json!({"status": "draft"});
    let result = runner.call_display_condition("hooks.field_hooks.condition_table", &data);
    assert!(result.is_some());
    match result.unwrap() {
        crap_cms::hooks::lifecycle::DisplayConditionResult::Table { visible, .. } => {
            assert!(
                !visible,
                "status=draft should not be visible when condition says equals=published"
            );
        }
        other => panic!("Expected Table, got {:?}", other),
    }
}

#[test]
fn call_display_condition_invalid_ref_returns_none() {
    let (_tmp, _pool, _registry, runner) = setup();

    let data = json!({"status": "published"});
    let result = runner.call_display_condition("hooks.nonexistent.function", &data);
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
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), json!("Test"));

    let ctx = HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
        user: None,
        ui_locale: None,
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
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

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
        r#"
        local M = {}
        function M.up() end
        return M
    "#,
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

#[test]
fn run_job_handler_with_valid_function() {
    let (_tmp, pool, _registry, runner) = setup();

    // Add a simple job handler Lua function
    let conn = pool.get().expect("DB connection");

    // We can test using eval_lua_with_conn to define a function, then call run_job_handler.
    // But run_job_handler resolves a function ref, so we need to write it to a Lua file.
    // Instead, let's use the field_hooks module which is already loaded.
    // We'll add a simple handler to the field_hooks module.

    // Actually, let's just test that run_job_handler works with a function that's already loaded.
    // The system_init function in field_hooks takes a context table and returns it.
    let result = runner.run_job_handler(
        "hooks.field_hooks.system_init",
        "test-job",
        r#"{"key": "value"}"#,
        1,
        3,
        &conn,
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
    let conn = pool.get().expect("DB connection");

    let result = runner.run_job_handler("hooks.nonexistent.handler", "test-job", "{}", 1, 3, &conn);
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
        .registry(registry.clone())
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
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").expect("articles");
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
        r#"
        local M = {}
        function M.run(ctx)
            return { processed = true, slug = ctx.job.slug, data_value = ctx.data.key }
        end
        return M
    "#,
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

    let conn = pool.get().expect("conn");
    let result = runner
        .run_job_handler(
            "jobs.test_job.run",
            "test-job",
            r#"{"key": "hello"}"#,
            1,
            3,
            &conn,
        )
        .expect("run_job_handler failed");

    assert!(result.is_some(), "Job should return a value");
    let result_json: serde_json::Value =
        serde_json::from_str(&result.unwrap()).expect("parse JSON");
    assert_eq!(
        result_json.get("processed").and_then(|v| v.as_bool()),
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
        r#"
        local M = {}
        function M.run(ctx)
            -- do nothing, return nil
        end
        return M
    "#,
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

    let conn = pool.get().expect("conn");
    let result = runner
        .run_job_handler("jobs.void_job.run", "void-job", "{}", 1, 1, &conn)
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
    let result = runner.call_display_condition("hooks.conditions.show_if_published", &form_data);
    match result {
        Some(crap_cms::hooks::lifecycle::DisplayConditionResult::Bool(b)) => assert!(b),
        other => panic!("Expected Bool(true), got {:?}", other),
    }

    let form_data_draft = json!({ "status": "draft" });
    let result =
        runner.call_display_condition("hooks.conditions.show_if_published", &form_data_draft);
    match result {
        Some(crap_cms::hooks::lifecycle::DisplayConditionResult::Bool(b)) => assert!(!b),
        other => panic!("Expected Bool(false), got {:?}", other),
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
    let result = runner.call_display_condition("hooks.conditions.condition_table", &form_data);
    match result {
        Some(crap_cms::hooks::lifecycle::DisplayConditionResult::Table { condition, visible }) => {
            assert!(visible, "status=published should match the condition");
            assert_eq!(
                condition.get("field").and_then(|v| v.as_str()),
                Some("status")
            );
            assert_eq!(
                condition.get("equals").and_then(|v| v.as_str()),
                Some("published")
            );
        }
        other => panic!("Expected Table result, got {:?}", other),
    }
}
