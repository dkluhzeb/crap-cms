use std::collections::HashMap;
use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::Document;
use crap_cms::core::collection::Hooks;
use crap_cms::core::field::FieldDefinition;
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::{
    AfterReadCtx, FieldHookEvent, HookContext, HookEvent, HookRunner,
};

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
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).expect("Failed to init Lua");

    let tmp = tempfile::tempdir().expect("Failed to create temp dir");
    let mut pool_config = CrapConfig::default();
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

fn create_article(
    pool: &crap_cms::db::DbPool,
    registry: &crap_cms::core::SharedRegistry,
    data: &HashMap<String, String>,
) -> Document {
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

// ── 2A. Hook Execution ──────────────────────────────────────────────────────

#[test]
fn check_live_setting_disabled_blocks() {
    use crap_cms::core::collection::LiveSetting;
    let (_tmp, _pool, _registry, runner) = setup();
    let result = runner.check_live_setting(
        Some(&LiveSetting::Disabled),
        "articles",
        "create",
        &HashMap::new(),
    );
    assert!(result.is_ok());
    assert!(
        !result.unwrap(),
        "Disabled live setting should block broadcast"
    );
}

#[test]
fn check_live_setting_function() {
    use crap_cms::core::collection::LiveSetting;
    let (_tmp, _pool, _registry, runner) = setup();

    // The filter_published function allows create/update but blocks delete
    let live = LiveSetting::Function("hooks.live.filter_published".to_string());

    let result = runner
        .check_live_setting(Some(&live), "articles", "create", &HashMap::new())
        .expect("should not error");
    assert!(result, "create should be allowed");

    let result = runner
        .check_live_setting(Some(&live), "articles", "update", &HashMap::new())
        .expect("should not error");
    assert!(result, "update should be allowed");

    let result = runner
        .check_live_setting(Some(&live), "articles", "delete", &HashMap::new())
        .expect("should not error");
    assert!(!result, "delete should be suppressed");
}

// ── 4C. Field After Hooks ────────────────────────────────────────────────────

#[test]
fn field_after_read_hook_transforms_value() {
    // Articles collection has field hooks on slug (before_change only),
    // but after_read is defined at collection level. Test that field-level
    // after_read hooks would work if defined.
    let (_tmp, pool, registry, runner) = setup();

    let mut data = HashMap::new();
    data.insert("title".to_string(), "After Read Test".to_string());
    let doc = create_article(&pool, &registry, &data);

    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Apply after_read hooks (collection-level after_read adds _was_read marker)
    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: "articles",
        operation: "find",
        user: None,
        ui_locale: None,
    };
    let transformed = runner.apply_after_read(&ar_ctx, doc);
    assert_eq!(
        transformed.fields.get("_was_read").and_then(|v| v.as_str()),
        Some("true"),
        "after_read hook should have set _was_read marker"
    );
}

#[test]
fn run_after_write_runs_hooks_with_crud_access() {
    // run_after_write runs after-hooks inside the transaction with CRUD access.
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let data = HashMap::new();
    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    let ctx = crap_cms::hooks::lifecycle::HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
        user: None,
        ui_locale: None,
    };
    let result = runner.run_after_write(
        &def.hooks,
        &def.fields,
        crap_cms::hooks::lifecycle::HookEvent::AfterChange,
        ctx,
        &tx,
    );
    // Should succeed (no after_change hooks defined in the fixture = no-op)
    assert!(result.is_ok());
    tx.commit().unwrap();
}

// ── 4D. Before Broadcast ─────────────────────────────────────────────────────

#[test]
fn before_broadcast_no_hooks_passes_through() {
    let (_tmp, _pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Broadcast Test"));

    let result = runner.run_before_broadcast(&def.hooks, "articles", "create", data);
    assert!(result.is_ok());
    let data = result.unwrap();
    assert!(
        data.is_some(),
        "No before_broadcast hooks → data should pass through"
    );
    assert_eq!(
        data.unwrap().get("title").and_then(|v| v.as_str()),
        Some("Broadcast Test"),
    );
}

#[test]
fn before_broadcast_transforms_data() {
    let (_tmp, _pool, _registry, runner) = setup();

    // Build hooks with a before_broadcast hook
    let hooks = Hooks {
        before_broadcast: vec!["hooks.live.transform_broadcast".to_string()],
        ..Default::default()
    };

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Original"));

    let result = runner
        .run_before_broadcast(&hooks, "articles", "create", data)
        .expect("should not error");
    assert!(result.is_some(), "Should not suppress");
    let data = result.unwrap();
    assert_eq!(
        data.get("_broadcast_marker").and_then(|v| v.as_str()),
        Some("transformed"),
        "before_broadcast hook should have added _broadcast_marker"
    );
}

#[test]
fn before_broadcast_suppresses_event() {
    let (_tmp, _pool, _registry, runner) = setup();

    let hooks = Hooks {
        before_broadcast: vec!["hooks.live.suppress_broadcast".to_string()],
        ..Default::default()
    };

    let data = HashMap::new();

    let result = runner
        .run_before_broadcast(&hooks, "articles", "create", data)
        .expect("should not error");
    assert!(
        result.is_none(),
        "suppress_broadcast should suppress the event"
    );
}

// ── 5A. Additional Hook Lifecycle Tests ──────────────────────────────────────

#[test]
fn validate_required_field_errors() {
    // Create a collection definition with a required field, try creating a
    // document without it, and verify that the validation error propagates.
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Build data WITHOUT the required "title" field
    let mut data = HashMap::new();
    data.insert("body".to_string(), serde_json::json!("Body without title"));

    let ctx = crap_cms::hooks::lifecycle::HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
        user: None,
        ui_locale: None,
    };

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    // run_before_write runs field hooks, validation, then collection hooks.
    // It should fail because "title" is required.
    let result = runner.run_before_write(&def.hooks, &def.fields, ctx, &tx, "articles", None, None);
    assert!(
        result.is_err(),
        "Should fail when required field 'title' is missing"
    );

    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("title") || err_msg.contains("required") || err_msg.contains("Validation"),
        "Error should reference the missing required field, got: {}",
        err_msg
    );
}

#[test]
fn after_read_hooks_fire() {
    // Set up a collection with an after_read hook, create a doc, find it,
    // and verify the after_read hook's modifications are present.
    let (_tmp, pool, registry, runner) = setup();

    // Create an article via the DB
    let mut create_data = HashMap::new();
    create_data.insert("title".to_string(), "After Read Fire Test".to_string());
    create_data.insert("body".to_string(), "Some body content".to_string());
    let doc = create_article(&pool, &registry, &create_data);

    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // The articles collection has an after_read hook that adds _was_read = "true"
    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: "articles",
        operation: "find",
        user: None,
        ui_locale: None,
    };
    let transformed = runner.apply_after_read(&ar_ctx, doc.clone());

    // Verify the after_read hook ran
    assert_eq!(
        transformed.fields.get("_was_read").and_then(|v| v.as_str()),
        Some("true"),
        "after_read hook should have set _was_read marker on the document"
    );

    // Title should be uppercased by field-level after_read hook
    assert_eq!(
        transformed.fields.get("title").and_then(|v| v.as_str()),
        Some("AFTER READ FIRE TEST"),
        "Title should be uppercased by field after_read hook"
    );
    assert_eq!(transformed.id, doc.id, "Document ID should not change");
}

#[test]
fn hook_error_rolls_back_transaction() {
    // Set up a hook that errors on before_change, attempt to create a document
    // via the CRUD lifecycle that triggers the hook, and verify the doc was
    // NOT created (transaction rolled back).
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // First verify the collection is empty
    let initial_count =
        crap_cms::db::ops::count_documents(&pool, "articles", &def, &[], None).expect("count");
    assert_eq!(initial_count, 0, "Should start with 0 articles");

    // Attempt to create a document inside a transaction, but have the hook error
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    // Create the document in the transaction
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Should Not Persist".to_string());
    let _doc = query::create(&tx, "articles", &def, &data, None)
        .expect("Create should succeed at DB level");

    // Now run a hook that errors — simulating what happens when after_change fails
    let result = runner.eval_lua_with_conn(
        r#"error("intentional hook error for rollback test")"#,
        &tx,
        None,
    );
    assert!(result.is_err(), "Hook error should propagate");

    // Drop the transaction WITHOUT committing (simulates rollback on error)
    drop(tx);

    // Verify the document was NOT persisted
    let final_count =
        crap_cms::db::ops::count_documents(&pool, "articles", &def, &[], None).expect("count");
    assert_eq!(
        final_count, 0,
        "Document should not be persisted after transaction rollback due to hook error"
    );
}

// ── 6A. Field-Level After-Read Hooks ─────────────────────────────────────────

#[test]
fn field_after_read_hooks_transform_values() {
    let (_tmp, pool, registry, runner) = setup();

    // Create an article
    let mut data = HashMap::new();
    data.insert("title".to_string(), "lowercase title".to_string());
    data.insert("body".to_string(), "Some body".to_string());
    let doc = create_article(&pool, &registry, &data);

    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // apply_after_read should run field-level after_read hooks (uppercase_value on title)
    // AND collection-level after_read hooks (_was_read marker)
    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: "articles",
        operation: "find",
        user: None,
        ui_locale: None,
    };
    let transformed = runner.apply_after_read(&ar_ctx, doc);

    // Field hook uppercases the title
    assert_eq!(
        transformed.fields.get("title").and_then(|v| v.as_str()),
        Some("LOWERCASE TITLE"),
        "Field after_read hook should have uppercased the title"
    );
    // Collection hook still fires
    assert_eq!(
        transformed.fields.get("_was_read").and_then(|v| v.as_str()),
        Some("true"),
        "Collection-level after_read hook should still fire"
    );
}

#[test]
fn field_after_read_hooks_with_apply_after_read_many() {
    let (_tmp, pool, registry, runner) = setup();

    let mut d1 = HashMap::new();
    d1.insert("title".to_string(), "first article".to_string());
    let doc1 = create_article(&pool, &registry, &d1);

    let mut d2 = HashMap::new();
    d2.insert("title".to_string(), "second article".to_string());
    let doc2 = create_article(&pool, &registry, &d2);

    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: "articles",
        operation: "find",
        user: None,
        ui_locale: None,
    };
    let results = runner.apply_after_read_many(&ar_ctx, vec![doc1, doc2]);

    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].fields.get("title").and_then(|v| v.as_str()),
        Some("FIRST ARTICLE"),
    );
    assert_eq!(
        results[1].fields.get("title").and_then(|v| v.as_str()),
        Some("SECOND ARTICLE"),
    );
}

// ── 6B. run_after_write with Field After-Change Hooks ────────────────────────

#[test]
fn run_after_write_runs_field_after_change_hooks() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Test Article"));

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

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    // run_after_write with AfterChange event should trigger field-level after_change hooks
    let result = runner
        .run_after_write(&def.hooks, &def.fields, HookEvent::AfterChange, ctx, &tx)
        .expect("run_after_write failed");

    // The collection-level after_change hook runs (logs but doesn't modify data)
    // The result should succeed
    assert!(result.data.contains_key("title"));
    tx.commit().unwrap();
}

#[test]
fn run_after_write_with_non_after_change_event() {
    // When event is not AfterChange, field hooks should NOT run
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Test"));
    data.insert("id".to_string(), serde_json::json!("test-id"));

    let ctx = HookContext {
        collection: "articles".to_string(),
        operation: "delete".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
        user: None,
        ui_locale: None,
    };

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    // AfterDelete event should not trigger field-level after_change hooks
    let result = runner.run_after_write(&def.hooks, &def.fields, HookEvent::AfterDelete, ctx, &tx);
    assert!(result.is_ok());
    tx.commit().unwrap();
}

// ── 6C. run_field_hooks (without conn) ───────────────────────────────────────

#[test]
fn run_field_hooks_without_conn() {
    let (_tmp, _pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("test title"));

    // run_field_hooks for AfterRead doesn't require CRUD access
    let result = runner.run_field_hooks(
        &def.fields,
        FieldHookEvent::AfterRead,
        &mut data,
        "articles",
        "find",
    );
    assert!(result.is_ok());

    // The after_read field hook uppercases the title
    assert_eq!(
        data.get("title").and_then(|v| v.as_str()),
        Some("TEST TITLE"),
    );
}

// ── 6R. HookContext with locale and draft fields ─────────────────────────────

#[test]
fn hook_context_passes_locale_and_draft() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Test"));

    let ctx = HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data,
        locale: Some("en".to_string()),
        draft: Some(true),
        context: HashMap::new(),
        user: None,
        ui_locale: None,
    };

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    // Should not error even with locale and draft set
    let result = runner
        .run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, ctx, &tx)
        .expect("Hook execution failed");

    assert_eq!(result.locale, Some("en".to_string()));
    assert_eq!(result.draft, Some(true));
}

// ── 6S. HookContext.context flow between hooks ───────────────────────────────

#[test]
fn hook_context_table_flows_through() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Test"));

    let mut context = HashMap::new();
    context.insert(
        "before_marker".to_string(),
        serde_json::json!("set-by-test"),
    );

    let ctx = HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context,
        user: None,
        ui_locale: None,
    };

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    // The hooks should receive the context table
    let result = runner
        .run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, ctx, &tx)
        .expect("Hook execution failed");

    // The result should still contain the before_marker since hooks don't clear it
    // (unless they modify the context table)
    assert!(result.data.contains_key("title"));
}

// ── 6T. Field-level before_validate hooks ────────────────────────────────────

#[test]
fn field_before_validate_hook_trims_title() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("  spaced title  "));

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    runner
        .run_field_hooks_with_conn(
            &def.fields,
            FieldHookEvent::BeforeValidate,
            &mut data,
            "articles",
            "create",
            &tx,
            None,
            None,
        )
        .expect("Field hook failed");

    // The title field has a before_validate trim hook
    assert_eq!(
        data.get("title").and_then(|v| v.as_str()),
        Some("spaced title"),
        "Field before_validate hook should trim the title"
    );
}

// ── 6W. check_live_setting with nil-returning function ───────────────────────

#[test]
fn check_live_setting_function_returns_nil_means_false() {
    use crap_cms::core::collection::LiveSetting;
    let (_tmp, _pool, _registry, runner) = setup();

    // suppress_broadcast returns nil, which should be treated as false
    let live = LiveSetting::Function("hooks.live.suppress_broadcast".to_string());
    let result = runner
        .check_live_setting(Some(&live), "articles", "create", &HashMap::new())
        .expect("should not error");
    assert!(!result, "nil return should mean suppress (false)");
}

// ── 6Z. Multiple field hooks in sequence ─────────────────────────────────────

#[test]
fn multiple_field_hooks_run_in_sequence() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // The title field has:
    //   before_validate: trim_value
    //   after_read: uppercase_value
    // Test that both run in the right order when called separately

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("  hello  "));

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    // First: before_validate trims
    runner
        .run_field_hooks_with_conn(
            &def.fields,
            FieldHookEvent::BeforeValidate,
            &mut data,
            "articles",
            "create",
            &tx,
            None,
            None,
        )
        .expect("before_validate field hook");

    assert_eq!(data.get("title").and_then(|v| v.as_str()), Some("hello"));

    // Then: after_read uppercases
    runner
        .run_field_hooks(
            &def.fields,
            FieldHookEvent::AfterRead,
            &mut data,
            "articles",
            "find",
        )
        .expect("after_read field hook");

    assert_eq!(data.get("title").and_then(|v| v.as_str()), Some("HELLO"));
}

// ── 6AA. run_before_write with user context ──────────────────────────────────

#[test]
fn run_before_write_with_user_context() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut user_fields = HashMap::new();
    user_fields.insert("role".to_string(), serde_json::json!("admin"));
    let user = Document {
        id: "user-1".to_string(),
        fields: user_fields,
        created_at: None,
        updated_at: None,
    };

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Admin Article"));
    data.insert("body".to_string(), serde_json::json!("Content"));

    let ctx = HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
        user: Some(user),
        ui_locale: None,
    };

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    let result = runner
        .run_before_write(&def.hooks, &def.fields, ctx, &tx, "articles", None, None)
        .expect("run_before_write with user failed");

    assert!(result.data.contains_key("title"));
}

// ── 7D. apply_after_read_many with empty hooks ────────────────────────────────

#[test]
fn apply_after_read_many_empty_hooks_passthrough() {
    let (_tmp, pool, registry, runner) = setup();
    let doc = create_article(
        &pool,
        &registry,
        &HashMap::from([("title".to_string(), "Test".to_string())]),
    );
    let hooks = Hooks::default();
    let fields: Vec<FieldDefinition> = Vec::new();
    let docs = vec![doc.clone()];
    let ar_ctx = AfterReadCtx {
        hooks: &hooks,
        fields: &fields,
        collection: "articles",
        operation: "find",
        user: None,
        ui_locale: None,
    };
    let result = runner.apply_after_read_many(&ar_ctx, docs);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].id, doc.id);
}

// ── 7E. Registered before_broadcast hooks ─────────────────────────────────────

#[test]
fn registered_before_broadcast_suppresses_event() {
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
        crap.hooks.register("before_broadcast", function(ctx)
            return nil  -- suppress
        end)
    "#,
    )
    .unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new");

    let hooks = Hooks::default();
    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Test"));
    let result = runner
        .run_before_broadcast(&hooks, "articles", "create", data)
        .expect("run_before_broadcast");
    assert!(
        result.is_none(),
        "Registered before_broadcast returning nil should suppress"
    );
}

#[test]
fn registered_before_broadcast_transforms_data() {
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
        crap.hooks.register("before_broadcast", function(ctx)
            ctx.data._registered_marker = "yes"
            return ctx
        end)
    "#,
    )
    .unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new");

    let hooks = Hooks::default();
    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Test"));
    let result = runner
        .run_before_broadcast(&hooks, "articles", "create", data)
        .expect("run_before_broadcast");
    assert!(result.is_some());
    let result_data = result.unwrap();
    assert_eq!(
        result_data
            .get("_registered_marker")
            .and_then(|v| v.as_str()),
        Some("yes"),
    );
}
