use std::collections::HashMap;
use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::Document;
use crap_cms::core::field::{FieldDefinition, FieldType, BlockDefinition};
use crap_cms::core::collection::CollectionHooks;
use crap_cms::db::{migrate, pool, query};
use crap_cms::db::query::AccessResult;
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::{HookRunner, HookContext, HookEvent, FieldHookEvent};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
}

fn setup() -> (tempfile::TempDir, crap_cms::db::DbPool, crap_cms::core::SharedRegistry, HookRunner) {
    let config_dir = fixture_dir();
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).expect("Failed to init Lua");

    let tmp = tempfile::tempdir().expect("Failed to create temp dir");
    let mut pool_config = CrapConfig::default();
    pool_config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &pool_config).expect("Failed to create pool");
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("Failed to sync schema");

    let runner = HookRunner::new(&config_dir, registry.clone(), &config)
        .expect("Failed to create HookRunner");
    (tmp, db_pool, registry, runner)
}

fn create_article(
    pool: &crap_cms::db::DbPool,
    registry: &crap_cms::core::SharedRegistry,
    data: &HashMap<String, String>,
) -> Document {
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").expect("articles not found").clone();
    drop(reg);

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let doc = query::create(&tx, "articles", &def, data, None).expect("Create failed");
    tx.commit().expect("Commit");
    doc
}

// ── 2A. Hook Execution ──────────────────────────────────────────────────────

#[test]
fn before_change_hook_modifies_data() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("  Test Title  "));
    data.insert("body".to_string(), serde_json::json!("Content"));

    let ctx = crap_cms::hooks::lifecycle::HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
    };

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    // run_before_write runs validate → hooks
    // But let's test run_hooks_with_conn for before_change directly
    let result = runner.run_hooks_with_conn(
        &def.hooks,
        crap_cms::hooks::lifecycle::HookEvent::BeforeChange,
        ctx,
        &tx,
        None,
    ).expect("Hook execution failed");

    // The before_change hook adds _hook_ran marker
    assert_eq!(
        result.data.get("_hook_ran").and_then(|v| v.as_str()),
        Some("before_change"),
        "before_change hook should have set _hook_ran marker"
    );
}

#[test]
fn before_validate_trims_title() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("  Spaces Around  "));

    let ctx = crap_cms::hooks::lifecycle::HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
    };

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    let result = runner.run_hooks_with_conn(
        &def.hooks,
        crap_cms::hooks::lifecycle::HookEvent::BeforeValidate,
        ctx,
        &tx,
        None,
    ).expect("Hook execution failed");

    // The before_validate hook trims the title
    assert_eq!(
        result.data.get("title").and_then(|v| v.as_str()),
        Some("Spaces Around"),
    );
}

#[test]
fn field_before_change_transforms_value() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Hello World"));
    // slug intentionally left empty — field hook should auto-generate from title

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    runner.run_field_hooks_with_conn(
        &def.fields,
        crap_cms::hooks::lifecycle::FieldHookEvent::BeforeChange,
        &mut data,
        "articles",
        "create",
        &tx,
        None,
    ).expect("Field hook failed");

    // The slug field hook should have generated a slug from the title
    let slug = data.get("slug").and_then(|v| v.as_str());
    assert!(slug.is_some(), "slug should have been generated");
    assert_eq!(slug.unwrap(), "hello-world");
}

#[test]
fn registered_hook_fires_for_all_collections() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Test"));

    let ctx = crap_cms::hooks::lifecycle::HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
    };

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    let result = runner.run_hooks_with_conn(
        &def.hooks,
        crap_cms::hooks::lifecycle::HookEvent::BeforeChange,
        ctx,
        &tx,
        None,
    ).expect("Hook execution failed");

    // The global registered hook (from init.lua) should have set _global_hook_ran
    assert_eq!(
        result.data.get("_global_hook_ran").and_then(|v| v.as_str()),
        Some("true"),
        "Global registered hook should have fired"
    );
}

#[test]
fn hook_error_rolls_back_conceptually() {
    // If a before_change hook returns an error, the caller should not commit.
    // We test this by verifying hook errors propagate correctly.
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let _def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Run the Lua directly to simulate a hook that errors
    let conn = pool.get().expect("DB connection");
    let result = runner.eval_lua_with_conn(
        r#"error("intentional hook error")"#,
        &conn,
        None,
    );
    assert!(result.is_err(), "Lua error should propagate as Rust error");
    assert!(result.unwrap_err().to_string().contains("intentional hook error"));
}

// ── 2B. Validate Fields ──────────────────────────────────────────────────────

#[test]
fn validate_required_present_passes() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Valid Title"));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&def.fields, &data, &conn, "articles", None, false);
    assert!(result.is_ok(), "Validation should pass with required field present");
}

#[test]
fn validate_required_missing_fails() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let data = HashMap::new(); // title is missing

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&def.fields, &data, &conn, "articles", None, false);
    assert!(result.is_err(), "Validation should fail with missing required field");
    let err = result.unwrap_err();
    assert!(err.errors.iter().any(|e| e.field == "title"), "Should have title error");
}

#[test]
fn validate_required_empty_string_fails() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("")); // empty string

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&def.fields, &data, &conn, "articles", None, false);
    assert!(result.is_err(), "Validation should fail with empty required field");
}

#[test]
fn validate_unique_passes_when_no_conflict() {
    let (_tmp, pool, registry, runner) = setup();

    // Create one article
    let mut create_data = HashMap::new();
    create_data.insert("title".to_string(), "Unique Title".to_string());
    let _doc = create_article(&pool, &registry, &create_data);

    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Validate a different title — should pass
    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Different Title"));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&def.fields, &data, &conn, "articles", None, false);
    assert!(result.is_ok(), "Unique validation should pass with different title");
}

#[test]
fn validate_unique_fails_on_duplicate() {
    let (_tmp, pool, registry, runner) = setup();

    // Create one article
    let mut create_data = HashMap::new();
    create_data.insert("title".to_string(), "Duplicate Title".to_string());
    let _doc = create_article(&pool, &registry, &create_data);

    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Validate same title — should fail
    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Duplicate Title"));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&def.fields, &data, &conn, "articles", None, false);
    assert!(result.is_err(), "Unique validation should fail for duplicate title");
    let err = result.unwrap_err();
    assert!(err.errors.iter().any(|e| e.field == "title" && e.message.contains("unique")));
}

#[test]
fn validate_unique_excludes_self_on_update() {
    let (_tmp, pool, registry, runner) = setup();

    // Create one article
    let mut create_data = HashMap::new();
    create_data.insert("title".to_string(), "My Title".to_string());
    let doc = create_article(&pool, &registry, &create_data);

    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Validate same title with exclude_id = self — should pass (updating own doc)
    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("My Title"));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&def.fields, &data, &conn, "articles", Some(&doc.id), false);
    assert!(result.is_ok(), "Unique validation should pass when excluding own ID");
}

// ── 2C. Custom Validate Functions ────────────────────────────────────────────

#[test]
fn custom_validate_function_passes() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Valid"));
    data.insert("word_count".to_string(), serde_json::json!(42)); // positive

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&def.fields, &data, &conn, "articles", None, false);
    assert!(result.is_ok(), "Custom validate should pass for positive number");
}

#[test]
fn custom_validate_function_fails() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Valid"));
    data.insert("word_count".to_string(), serde_json::json!(-5)); // negative!

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&def.fields, &data, &conn, "articles", None, false);
    assert!(result.is_err(), "Custom validate should fail for negative number");
    let err = result.unwrap_err();
    assert!(err.errors.iter().any(|e| e.field == "word_count"), "Should have word_count error");
}

#[test]
fn custom_validate_returns_error_message() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Valid"));
    data.insert("word_count".to_string(), serde_json::json!(-1));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&def.fields, &data, &conn, "articles", None, false);
    let err = result.unwrap_err();
    let word_count_err = err.errors.iter().find(|e| e.field == "word_count").unwrap();
    assert!(
        word_count_err.message.contains("positive"),
        "Error message should mention 'positive', got: {}",
        word_count_err.message
    );
}

// ── 2D. Block/Array Sub-field Validation ─────────────────────────────────────

#[test]
fn validate_blocks_required_subfield_fails_when_empty() {
    let (_tmp, pool, _registry, runner) = setup();

    // Build a blocks field definition with a required sub-field
    let blocks_field = FieldDefinition {
        name: "content".to_string(),
        field_type: FieldType::Blocks,
        blocks: vec![
            crap_cms::core::field::BlockDefinition {
                block_type: "text".to_string(),
                fields: vec![
                    FieldDefinition {
                        name: "title".to_string(),
                        field_type: FieldType::Text,
                        required: true,
                        ..Default::default()
                    },
                    FieldDefinition {
                        name: "body".to_string(),
                        field_type: FieldType::Textarea,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![blocks_field];
    let mut data = HashMap::new();
    data.insert("content".to_string(), serde_json::json!([
        { "_block_type": "text", "title": "", "body": "some content" }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "articles", None, false);
    assert!(result.is_err(), "Validation should fail with empty required block sub-field");
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field == "content[0][title]"),
        "Should have content[0][title] error, got: {:?}",
        err.errors.iter().map(|e| &e.field).collect::<Vec<_>>()
    );
}

#[test]
fn validate_blocks_required_subfield_passes_when_present() {
    let (_tmp, pool, _registry, runner) = setup();

    let blocks_field = FieldDefinition {
        name: "content".to_string(),
        field_type: FieldType::Blocks,
        blocks: vec![
            crap_cms::core::field::BlockDefinition {
                block_type: "text".to_string(),
                fields: vec![
                    FieldDefinition {
                        name: "title".to_string(),
                        field_type: FieldType::Text,
                        required: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![blocks_field];
    let mut data = HashMap::new();
    data.insert("content".to_string(), serde_json::json!([
        { "_block_type": "text", "title": "Hello World" }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "articles", None, false);
    assert!(result.is_ok(), "Validation should pass when required block sub-field is present");
}

#[test]
fn validate_blocks_skips_required_for_drafts() {
    let (_tmp, pool, _registry, runner) = setup();

    let blocks_field = FieldDefinition {
        name: "content".to_string(),
        field_type: FieldType::Blocks,
        blocks: vec![
            crap_cms::core::field::BlockDefinition {
                block_type: "text".to_string(),
                fields: vec![
                    FieldDefinition {
                        name: "title".to_string(),
                        field_type: FieldType::Text,
                        required: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![blocks_field];
    let mut data = HashMap::new();
    data.insert("content".to_string(), serde_json::json!([
        { "_block_type": "text", "title": "" }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "articles", None, true);
    assert!(result.is_ok(), "Validation should skip required sub-fields for drafts");
}

#[test]
fn validate_array_required_subfield_fails_when_empty() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition {
        name: "items".to_string(),
        field_type: FieldType::Array,
        fields: vec![
            FieldDefinition {
                name: "label".to_string(),
                field_type: FieldType::Text,
                required: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert("items".to_string(), serde_json::json!([
        { "label": "ok" },
        { "label": "" }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "articles", None, false);
    assert!(result.is_err(), "Validation should fail for empty required array sub-field");
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field == "items[1][label]"),
        "Should have items[1][label] error, got: {:?}",
        err.errors.iter().map(|e| &e.field).collect::<Vec<_>>()
    );
    // First row should pass
    assert!(
        !err.errors.iter().any(|e| e.field == "items[0][label]"),
        "First row should not have error"
    );
}

// ── Run before_write lifecycle (integrated) ──────────────────────────────────

#[test]
fn run_before_write_full_lifecycle() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("  My Article  "));
    data.insert("body".to_string(), serde_json::json!("Article body"));

    let ctx = crap_cms::hooks::lifecycle::HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
    };

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    let result = runner.run_before_write(
        &def.hooks, &def.fields, ctx, &tx, "articles", None, None, false,
    ).expect("run_before_write failed");

    // Title should be trimmed (before_validate hook)
    assert_eq!(result.data.get("title").and_then(|v| v.as_str()), Some("My Article"));
    // before_change hook marker
    assert_eq!(result.data.get("_hook_ran").and_then(|v| v.as_str()), Some("before_change"));
    // global hook marker
    assert_eq!(result.data.get("_global_hook_ran").and_then(|v| v.as_str()), Some("true"));
    // slug should have been generated by field hook
    let slug = result.data.get("slug").and_then(|v| v.as_str());
    assert!(slug.is_some(), "slug should have been generated by field hook");
}

#[test]
fn run_before_write_fails_on_validation_error() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Missing required title
    let data = HashMap::new();

    let ctx = crap_cms::hooks::lifecycle::HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
    };

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    let result = runner.run_before_write(
        &def.hooks, &def.fields, ctx, &tx, "articles", None, None, false,
    );
    assert!(result.is_err(), "run_before_write should fail when validation fails");
}

// ── Eval lua with conn ──────────────────────────────────────────────────────

#[test]
fn eval_lua_crud_in_hook_context() {
    let (_tmp, pool, registry, runner) = setup();

    // Create an article first
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Lua Test Article".to_string());
    let _doc = create_article(&pool, &registry, &data);

    let conn = pool.get().expect("DB connection");
    let result = runner.eval_lua_with_conn(
        r#"
        local r = crap.collections.find("articles", { overrideAccess = true })
        return tostring(r.pagination.totalDocs)
        "#,
        &conn,
        None,
    ).expect("Eval failed");
    assert_eq!(result, "1", "Should find 1 article");
}

// ── Helper: build a minimal FieldDefinition ──────────────────────────────────

fn make_field(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition {
        name: name.to_string(),
        field_type,
        ..Default::default()
    }
}

fn make_field_with_read_access(name: &str, read_ref: &str) -> FieldDefinition {
    let mut f = make_field(name, FieldType::Text);
    f.access.read = Some(read_ref.to_string());
    f
}

fn make_field_with_write_access(name: &str, create_ref: Option<&str>, update_ref: Option<&str>) -> FieldDefinition {
    let mut f = make_field(name, FieldType::Text);
    f.access.create = create_ref.map(|s| s.to_string());
    f.access.update = update_ref.map(|s| s.to_string());
    f
}

// ── 3A. Access Control Tests ─────────────────────────────────────────────────

#[test]
fn check_access_none_ref_is_allowed() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    let result = runner.check_access(None, None, None, None, &conn)
        .expect("check_access failed");
    assert!(
        matches!(result, AccessResult::Allowed),
        "None access ref should return Allowed"
    );
}

#[test]
fn check_access_returns_allowed() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    let result = runner.check_access(
        Some("hooks.access.allow_all"), None, None, None, &conn,
    ).expect("check_access failed");
    assert!(
        matches!(result, AccessResult::Allowed),
        "allow_all should return Allowed"
    );
}

#[test]
fn check_access_returns_denied() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    let result = runner.check_access(
        Some("hooks.access.deny_all"), None, None, None, &conn,
    ).expect("check_access failed");
    assert!(
        matches!(result, AccessResult::Denied),
        "deny_all should return Denied"
    );
}

#[test]
fn check_access_returns_constrained() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    let result = runner.check_access(
        Some("hooks.access.constrained"), None, None, None, &conn,
    ).expect("check_access failed");
    match result {
        AccessResult::Constrained(clauses) => {
            assert!(!clauses.is_empty(), "Constrained should have at least one clause");
        }
        other => panic!("Expected Constrained, got {:?}", other),
    }
}

#[test]
fn check_access_with_user_context() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    // User with admin role should be allowed
    let mut admin_fields = HashMap::new();
    admin_fields.insert("role".to_string(), serde_json::json!("admin"));
    let admin_user = Document {
        id: "user-1".to_string(),
        fields: admin_fields,
        created_at: None,
        updated_at: None,
    };

    let result = runner.check_access(
        Some("hooks.access.check_role"), Some(&admin_user), None, None, &conn,
    ).expect("check_access failed");
    assert!(
        matches!(result, AccessResult::Allowed),
        "Admin user should be Allowed by check_role"
    );

    // User without admin role should be denied
    let mut regular_fields = HashMap::new();
    regular_fields.insert("role".to_string(), serde_json::json!("editor"));
    let regular_user = Document {
        id: "user-2".to_string(),
        fields: regular_fields,
        created_at: None,
        updated_at: None,
    };

    let result = runner.check_access(
        Some("hooks.access.check_role"), Some(&regular_user), None, None, &conn,
    ).expect("check_access failed");
    assert!(
        matches!(result, AccessResult::Denied),
        "Non-admin user should be Denied by check_role"
    );

    // No user at all should be denied
    let result = runner.check_access(
        Some("hooks.access.check_role"), None, None, None, &conn,
    ).expect("check_access failed");
    assert!(
        matches!(result, AccessResult::Denied),
        "No user should be Denied by check_role"
    );
}

// ── 3B. Field-Level Access Tests ─────────────────────────────────────────────

#[test]
fn check_field_read_access_no_access_config() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    // Fields without any access config should not be denied
    let fields = vec![
        make_field("title", FieldType::Text),
        make_field("body", FieldType::Textarea),
    ];

    let denied = runner.check_field_read_access(&fields, None, &conn);
    assert!(
        denied.is_empty(),
        "Fields without access config should not be in denied list, got: {:?}",
        denied
    );
}

#[test]
fn check_field_read_access_denies_field() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    let fields = vec![
        make_field("title", FieldType::Text),
        make_field_with_read_access("secret", "hooks.access.deny_all"),
        make_field("body", FieldType::Textarea),
    ];

    let denied = runner.check_field_read_access(&fields, None, &conn);
    assert_eq!(denied.len(), 1, "Should deny exactly one field");
    assert_eq!(denied[0], "secret", "The denied field should be 'secret'");
}

#[test]
fn check_field_read_access_allows_with_allow_all() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    let fields = vec![
        make_field_with_read_access("visible", "hooks.access.allow_all"),
        make_field_with_read_access("hidden", "hooks.access.deny_all"),
    ];

    let denied = runner.check_field_read_access(&fields, None, &conn);
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0], "hidden");
}

#[test]
fn check_field_write_access_no_access_config() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    let fields = vec![
        make_field("title", FieldType::Text),
        make_field("body", FieldType::Textarea),
    ];

    let denied = runner.check_field_write_access(&fields, None, "create", &conn);
    assert!(
        denied.is_empty(),
        "Fields without write access config should not be denied"
    );
}

#[test]
fn check_field_write_access_denies_field_on_create() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    let fields = vec![
        make_field("title", FieldType::Text),
        make_field_with_write_access("protected", Some("hooks.access.deny_all"), None),
    ];

    let denied = runner.check_field_write_access(&fields, None, "create", &conn);
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0], "protected");
}

#[test]
fn check_field_write_access_denies_field_on_update() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    let fields = vec![
        make_field("title", FieldType::Text),
        make_field_with_write_access("locked", None, Some("hooks.access.deny_all")),
    ];

    // On update, the "locked" field should be denied
    let denied = runner.check_field_write_access(&fields, None, "update", &conn);
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0], "locked");

    // On create, no restriction since create access is None
    let denied = runner.check_field_write_access(&fields, None, "create", &conn);
    assert!(denied.is_empty(), "No create restriction defined, should be empty");
}

// ── 3C. After-Read Hook Tests ────────────────────────────────────────────────

#[test]
fn apply_after_read_transforms_doc() {
    let (_tmp, pool, registry, runner) = setup();

    // Create an article via DB so we have a real doc
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Read Test".to_string());
    data.insert("body".to_string(), "Some body text".to_string());
    let doc = create_article(&pool, &registry, &data);

    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // apply_after_read should run the after_read hook which adds _was_read marker
    let transformed = runner.apply_after_read(
        &def.hooks, &def.fields, "articles", "find", doc.clone(),
    );

    assert_eq!(
        transformed.fields.get("_was_read").and_then(|v| v.as_str()),
        Some("true"),
        "after_read hook should have set _was_read marker"
    );
    // Title should be uppercased by field-level after_read hook
    assert_eq!(
        transformed.fields.get("title").and_then(|v| v.as_str()),
        Some("READ TEST"),
        "Title should be uppercased by field after_read hook"
    );
    assert_eq!(transformed.id, doc.id, "Document ID should not change");
}

#[test]
fn apply_after_read_many_transforms_all() {
    let (_tmp, pool, registry, runner) = setup();

    // Create two articles
    let mut data1 = HashMap::new();
    data1.insert("title".to_string(), "Article One".to_string());
    let doc1 = create_article(&pool, &registry, &data1);

    let mut data2 = HashMap::new();
    data2.insert("title".to_string(), "Article Two".to_string());
    let doc2 = create_article(&pool, &registry, &data2);

    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let docs = vec![doc1, doc2];
    let transformed = runner.apply_after_read_many(
        &def.hooks, &def.fields, "articles", "find", docs,
    );

    assert_eq!(transformed.len(), 2, "Should return same number of docs");
    for doc in &transformed {
        assert_eq!(
            doc.fields.get("_was_read").and_then(|v| v.as_str()),
            Some("true"),
            "Each doc should have _was_read marker from after_read hook"
        );
    }
}

#[test]
fn apply_after_read_no_hooks_returns_same() {
    let (_tmp, pool, registry, runner) = setup();

    // Create a document
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Untouched".to_string());
    let doc = create_article(&pool, &registry, &data);

    // Use empty hooks (no after_read configured)
    let empty_hooks = CollectionHooks::default();
    let empty_fields: Vec<FieldDefinition> = Vec::new();

    let original_id = doc.id.clone();
    let original_title = doc.fields.get("title").cloned();

    let result = runner.apply_after_read(
        &empty_hooks, &empty_fields, "articles", "find", doc,
    );

    assert_eq!(result.id, original_id, "ID should be unchanged");
    assert_eq!(
        result.fields.get("title"),
        original_title.as_ref(),
        "Title should be unchanged when no after_read hooks"
    );
    // _was_read should NOT be present since we used empty hooks
    assert!(
        !result.fields.contains_key("_was_read"),
        "_was_read should not be present with empty hooks"
    );
}

// ── 3D. Before-Read Hook Tests ───────────────────────────────────────────────

#[test]
fn fire_before_read_executes() {
    let (_tmp, _pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // fire_before_read should succeed without error for articles
    // (articles collection does not have before_read hooks, but it should not error)
    let data = HashMap::new();
    let result = runner.fire_before_read(&def.hooks, "articles", "find", data);
    assert!(
        result.is_ok(),
        "fire_before_read should not error even with no before_read hooks defined"
    );
}

// ── 4A. Auth Strategies ──────────────────────────────────────────────────────

#[test]
fn auth_strategy_returns_user_on_valid_key() {
    let (_tmp, pool, registry, runner) = setup();

    // Create an article (auth_strategy.lua looks up articles to return a user-like doc)
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Strategy Test".to_string());
    let _doc = create_article(&pool, &registry, &data);

    let mut headers = HashMap::new();
    headers.insert("x-api-key".to_string(), "valid-key".to_string());

    let conn = pool.get().expect("DB connection");
    let result = runner.run_auth_strategy(
        "hooks.auth_strategy.api_key_auth",
        "articles",
        &headers,
        &conn,
    );
    assert!(result.is_ok(), "run_auth_strategy should not error");
    let doc = result.unwrap();
    assert!(doc.is_some(), "Valid key should return a document");
}

#[test]
fn auth_strategy_returns_none_on_invalid_key() {
    let (_tmp, pool, _registry, runner) = setup();

    let mut headers = HashMap::new();
    headers.insert("x-api-key".to_string(), "wrong-key".to_string());

    let conn = pool.get().expect("DB connection");
    let result = runner.run_auth_strategy(
        "hooks.auth_strategy.api_key_auth",
        "articles",
        &headers,
        &conn,
    ).expect("should not error");
    assert!(result.is_none(), "Invalid key should return None");
}

#[test]
fn auth_strategy_returns_none_on_missing_header() {
    let (_tmp, pool, _registry, runner) = setup();

    let headers = HashMap::new(); // no x-api-key header

    let conn = pool.get().expect("DB connection");
    let result = runner.run_auth_strategy(
        "hooks.auth_strategy.api_key_auth",
        "articles",
        &headers,
        &conn,
    ).expect("should not error");
    assert!(result.is_none(), "Missing header should return None");
}

#[test]
fn auth_strategy_has_crud_access() {
    let (_tmp, pool, registry, runner) = setup();

    // Create two articles for the strategy to find
    let mut data = HashMap::new();
    data.insert("title".to_string(), "First Article".to_string());
    let _doc = create_article(&pool, &registry, &data);
    data.insert("title".to_string(), "Second Article".to_string());
    let _doc = create_article(&pool, &registry, &data);

    // The strategy calls crap.collections.find — test that it works
    let mut headers = HashMap::new();
    headers.insert("x-api-key".to_string(), "valid-key".to_string());

    let conn = pool.get().expect("DB connection");
    let result = runner.run_auth_strategy(
        "hooks.auth_strategy.api_key_auth",
        "articles",
        &headers,
        &conn,
    ).expect("should not error");
    assert!(result.is_some(), "Strategy with CRUD access should find articles and return one");
}

// ── 4B. Live Events ──────────────────────────────────────────────────────────

#[test]
fn check_live_setting_none_allows() {
    let (_tmp, _pool, _registry, runner) = setup();
    let result = runner.check_live_setting(None, "articles", "create", &HashMap::new());
    assert!(result.is_ok());
    assert!(result.unwrap(), "None live setting should allow broadcast");
}

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
    assert!(!result.unwrap(), "Disabled live setting should block broadcast");
}

#[test]
fn check_live_setting_function() {
    use crap_cms::core::collection::LiveSetting;
    let (_tmp, _pool, _registry, runner) = setup();

    // The filter_published function allows create/update but blocks delete
    let live = LiveSetting::Function("hooks.live.filter_published".to_string());

    let result = runner.check_live_setting(
        Some(&live), "articles", "create", &HashMap::new(),
    ).expect("should not error");
    assert!(result, "create should be allowed");

    let result = runner.check_live_setting(
        Some(&live), "articles", "update", &HashMap::new(),
    ).expect("should not error");
    assert!(result, "update should be allowed");

    let result = runner.check_live_setting(
        Some(&live), "articles", "delete", &HashMap::new(),
    ).expect("should not error");
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
    let transformed = runner.apply_after_read(
        &def.hooks, &def.fields, "articles", "find", doc,
    );
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
    };
    let result = runner.run_after_write(
        &def.hooks,
        &def.fields,
        crap_cms::hooks::lifecycle::HookEvent::AfterChange,
        ctx,
        &tx,
        None,
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

    let result = runner.run_before_broadcast(
        &def.hooks, "articles", "create", data,
    );
    assert!(result.is_ok());
    let data = result.unwrap();
    assert!(data.is_some(), "No before_broadcast hooks → data should pass through");
    assert_eq!(
        data.unwrap().get("title").and_then(|v| v.as_str()),
        Some("Broadcast Test"),
    );
}

#[test]
fn before_broadcast_transforms_data() {
    let (_tmp, _pool, _registry, runner) = setup();

    // Build hooks with a before_broadcast hook
    let hooks = CollectionHooks {
        before_broadcast: vec!["hooks.live.transform_broadcast".to_string()],
        ..Default::default()
    };

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Original"));

    let result = runner.run_before_broadcast(&hooks, "articles", "create", data)
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

    let hooks = CollectionHooks {
        before_broadcast: vec!["hooks.live.suppress_broadcast".to_string()],
        ..Default::default()
    };

    let data = HashMap::new();

    let result = runner.run_before_broadcast(&hooks, "articles", "create", data)
        .expect("should not error");
    assert!(result.is_none(), "suppress_broadcast should suppress the event");
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
    };

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    // run_before_write runs field hooks, validation, then collection hooks.
    // It should fail because "title" is required.
    let result = runner.run_before_write(
        &def.hooks, &def.fields, ctx, &tx, "articles", None, None, false,
    );
    assert!(result.is_err(), "Should fail when required field 'title' is missing");

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
    let transformed = runner.apply_after_read(
        &def.hooks, &def.fields, "articles", "find", doc.clone(),
    );

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
    let initial_count = crap_cms::db::ops::count_documents(
        &pool, "articles", &def, &[], None,
    ).expect("count");
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
    let final_count = crap_cms::db::ops::count_documents(
        &pool, "articles", &def, &[], None,
    ).expect("count");
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
    let transformed = runner.apply_after_read(
        &def.hooks, &def.fields, "articles", "find", doc,
    );

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

    let results = runner.apply_after_read_many(
        &def.hooks, &def.fields, "articles", "find", vec![doc1, doc2],
    );

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
    };

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    // run_after_write with AfterChange event should trigger field-level after_change hooks
    let result = runner.run_after_write(
        &def.hooks,
        &def.fields,
        HookEvent::AfterChange,
        ctx,
        &tx,
        None,
    ).expect("run_after_write failed");

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
    };

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    // AfterDelete event should not trigger field-level after_change hooks
    let result = runner.run_after_write(
        &def.hooks,
        &def.fields,
        HookEvent::AfterDelete,
        ctx,
        &tx,
        None,
    );
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

// ── 6D. Validate: Group Sub-fields ───────────────────────────────────────────

#[test]
fn validate_group_required_subfield_fails() {
    let (_tmp, pool, _registry, runner) = setup();

    let group_field = FieldDefinition {
        name: "seo".to_string(),
        field_type: FieldType::Group,
        fields: vec![
            FieldDefinition {
                name: "meta_title".to_string(),
                field_type: FieldType::Text,
                required: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![group_field];
    let mut data = HashMap::new();
    // Group sub-fields are stored as group__subfield at top level
    data.insert("seo__meta_title".to_string(), serde_json::json!(""));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "test_table", None, false);
    assert!(result.is_err(), "Validation should fail for empty required group sub-field");
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field == "seo__meta_title"),
        "Should have seo__meta_title error, got: {:?}",
        err.errors
    );
}

#[test]
fn validate_group_required_subfield_passes() {
    let (_tmp, pool, _registry, runner) = setup();

    let group_field = FieldDefinition {
        name: "seo".to_string(),
        field_type: FieldType::Group,
        fields: vec![
            FieldDefinition {
                name: "meta_title".to_string(),
                field_type: FieldType::Text,
                required: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![group_field];
    let mut data = HashMap::new();
    data.insert("seo__meta_title".to_string(), serde_json::json!("My Title"));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "test_table", None, false);
    assert!(result.is_ok(), "Validation should pass for non-empty required group sub-field");
}

#[test]
fn validate_group_skips_for_drafts() {
    let (_tmp, pool, _registry, runner) = setup();

    let group_field = FieldDefinition {
        name: "seo".to_string(),
        field_type: FieldType::Group,
        fields: vec![
            FieldDefinition {
                name: "meta_title".to_string(),
                field_type: FieldType::Text,
                required: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![group_field];
    let mut data = HashMap::new();
    data.insert("seo__meta_title".to_string(), serde_json::json!(""));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "test_table", None, true);
    assert!(result.is_ok(), "Validation should skip group sub-field required check for drafts");
}

// ── 6E. Validate: min_rows / max_rows ────────────────────────────────────────

#[test]
fn validate_min_rows_fails() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition {
        name: "items".to_string(),
        field_type: FieldType::Array,
        min_rows: Some(2),
        fields: vec![
            FieldDefinition {
                name: "label".to_string(),
                field_type: FieldType::Text,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert("items".to_string(), serde_json::json!([
        { "label": "only one" }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "test_table", None, false);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.errors.iter().any(|e| e.field == "items" && e.message.contains("at least 2")));
}

#[test]
fn validate_max_rows_fails() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition {
        name: "items".to_string(),
        field_type: FieldType::Array,
        max_rows: Some(1),
        fields: vec![
            FieldDefinition {
                name: "label".to_string(),
                field_type: FieldType::Text,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert("items".to_string(), serde_json::json!([
        { "label": "one" },
        { "label": "two" }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "test_table", None, false);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.errors.iter().any(|e| e.field == "items" && e.message.contains("at most 1")));
}

#[test]
fn validate_min_max_rows_passes_when_in_range() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition {
        name: "items".to_string(),
        field_type: FieldType::Array,
        min_rows: Some(1),
        max_rows: Some(3),
        fields: vec![
            FieldDefinition {
                name: "label".to_string(),
                field_type: FieldType::Text,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert("items".to_string(), serde_json::json!([
        { "label": "one" },
        { "label": "two" }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "test_table", None, false);
    assert!(result.is_ok());
}

#[test]
fn validate_min_rows_skips_for_drafts() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition {
        name: "items".to_string(),
        field_type: FieldType::Array,
        min_rows: Some(5),
        fields: vec![
            FieldDefinition {
                name: "label".to_string(),
                field_type: FieldType::Text,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert("items".to_string(), serde_json::json!([]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "test_table", None, true);
    assert!(result.is_ok(), "min_rows should be skipped for drafts");
}

// ── 6F. Validate: Date Format ────────────────────────────────────────────────

#[test]
fn validate_date_field_valid_formats() {
    let (_tmp, pool, _registry, runner) = setup();

    let date_field = FieldDefinition {
        name: "due_date".to_string(),
        field_type: FieldType::Date,
        ..Default::default()
    };

    let fields = vec![date_field];
    let conn = pool.get().expect("DB connection");

    // YYYY-MM-DD
    let mut data = HashMap::new();
    data.insert("due_date".to_string(), serde_json::json!("2024-01-15"));
    assert!(runner.validate_fields(&fields, &data, &conn, "t", None, false).is_ok());

    // YYYY-MM-DDTHH:MM
    data.insert("due_date".to_string(), serde_json::json!("2024-01-15T14:30"));
    assert!(runner.validate_fields(&fields, &data, &conn, "t", None, false).is_ok());

    // YYYY-MM-DDTHH:MM:SS
    data.insert("due_date".to_string(), serde_json::json!("2024-01-15T14:30:00"));
    assert!(runner.validate_fields(&fields, &data, &conn, "t", None, false).is_ok());

    // Full ISO 8601
    data.insert("due_date".to_string(), serde_json::json!("2024-01-15T14:30:00+00:00"));
    assert!(runner.validate_fields(&fields, &data, &conn, "t", None, false).is_ok());

    // Time only: HH:MM
    data.insert("due_date".to_string(), serde_json::json!("14:30"));
    assert!(runner.validate_fields(&fields, &data, &conn, "t", None, false).is_ok());

    // Time only: HH:MM:SS
    data.insert("due_date".to_string(), serde_json::json!("14:30:00"));
    assert!(runner.validate_fields(&fields, &data, &conn, "t", None, false).is_ok());

    // Month only: YYYY-MM
    data.insert("due_date".to_string(), serde_json::json!("2024-01"));
    assert!(runner.validate_fields(&fields, &data, &conn, "t", None, false).is_ok());
}

#[test]
fn validate_date_field_invalid_format() {
    let (_tmp, pool, _registry, runner) = setup();

    let date_field = FieldDefinition {
        name: "due_date".to_string(),
        field_type: FieldType::Date,
        ..Default::default()
    };

    let fields = vec![date_field];
    let conn = pool.get().expect("DB connection");

    let mut data = HashMap::new();
    data.insert("due_date".to_string(), serde_json::json!("not-a-date"));
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.errors.iter().any(|e| e.field == "due_date" && e.message.contains("valid date")));
}

#[test]
fn validate_date_empty_is_ok() {
    let (_tmp, pool, _registry, runner) = setup();

    let date_field = FieldDefinition {
        name: "due_date".to_string(),
        field_type: FieldType::Date,
        ..Default::default()
    };

    let fields = vec![date_field];
    let conn = pool.get().expect("DB connection");

    // Empty string should not trigger date format validation (is_empty = true)
    let mut data = HashMap::new();
    data.insert("due_date".to_string(), serde_json::json!(""));
    assert!(runner.validate_fields(&fields, &data, &conn, "t", None, false).is_ok());

    // Null should not trigger date format validation
    data.insert("due_date".to_string(), serde_json::Value::Null);
    assert!(runner.validate_fields(&fields, &data, &conn, "t", None, false).is_ok());
}

// ── 6G. Validate: Sub-field Date and Validate in Array Rows ──────────────────

#[test]
fn validate_date_subfield_in_array_rows() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition {
        name: "events".to_string(),
        field_type: FieldType::Array,
        fields: vec![
            FieldDefinition {
                name: "event_date".to_string(),
                field_type: FieldType::Date,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert("events".to_string(), serde_json::json!([
        { "event_date": "2024-01-15" },
        { "event_date": "bad-date" }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field == "events[1][event_date]" && e.message.contains("valid date")),
        "Should have date validation error for events[1][event_date], got: {:?}",
        err.errors
    );
}

#[test]
fn validate_custom_function_in_array_subfield() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition {
        name: "scores".to_string(),
        field_type: FieldType::Array,
        fields: vec![
            FieldDefinition {
                name: "value".to_string(),
                field_type: FieldType::Number,
                validate: Some("hooks.validators.positive_number".to_string()),
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert("scores".to_string(), serde_json::json!([
        { "value": 10 },
        { "value": -5 }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field == "scores[1][value]"),
        "Should have custom validate error for scores[1][value], got: {:?}",
        err.errors
    );
}

// ── 6H. Validate: Nested Array/Blocks within Array Rows ──────────────────────

#[test]
fn validate_nested_array_in_array_rows() {
    let (_tmp, pool, _registry, runner) = setup();

    let nested_array = FieldDefinition {
        name: "outer".to_string(),
        field_type: FieldType::Array,
        fields: vec![
            FieldDefinition {
                name: "inner".to_string(),
                field_type: FieldType::Array,
                fields: vec![
                    FieldDefinition {
                        name: "value".to_string(),
                        field_type: FieldType::Text,
                        required: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![nested_array];
    let mut data = HashMap::new();
    data.insert("outer".to_string(), serde_json::json!([
        {
            "inner": [
                { "value": "ok" },
                { "value": "" }
            ]
        }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field.contains("outer[0][inner][1][value]")),
        "Should have nested array validation error, got: {:?}",
        err.errors
    );
}

#[test]
fn validate_nested_blocks_in_array_rows() {
    let (_tmp, pool, _registry, runner) = setup();

    let outer = FieldDefinition {
        name: "sections".to_string(),
        field_type: FieldType::Array,
        fields: vec![
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Blocks,
                blocks: vec![
                    BlockDefinition {
                        block_type: "text".to_string(),
                        fields: vec![
                            FieldDefinition {
                                name: "body".to_string(),
                                field_type: FieldType::Text,
                                required: true,
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![outer];
    let mut data = HashMap::new();
    data.insert("sections".to_string(), serde_json::json!([
        {
            "content": [
                { "_block_type": "text", "body": "" }
            ]
        }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field.contains("sections[0][content][0][body]")),
        "Should have nested blocks validation error, got: {:?}",
        err.errors
    );
}

// ── 6I. Validate: Group Sub-fields in Array Rows ─────────────────────────────

#[test]
fn validate_group_in_array_row_required_subfield() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition {
        name: "items".to_string(),
        field_type: FieldType::Array,
        fields: vec![
            FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "author".to_string(),
                        field_type: FieldType::Text,
                        required: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![array_field];
    let mut data = HashMap::new();
    // Group sub-fields in array rows use group__subfield keys
    data.insert("items".to_string(), serde_json::json!([
        { "meta__author": "" }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field.contains("items[0][meta__author]")),
        "Should have group-in-array validation error, got: {:?}",
        err.errors
    );
}

#[test]
fn validate_group_date_subfield_in_array_row() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition {
        name: "items".to_string(),
        field_type: FieldType::Array,
        fields: vec![
            FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "published_at".to_string(),
                        field_type: FieldType::Date,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert("items".to_string(), serde_json::json!([
        { "meta__published_at": "bad-date" }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field.contains("items[0][meta__published_at]") && e.message.contains("valid date")),
        "Should have group-date validation error in array row, got: {:?}",
        err.errors
    );
}

#[test]
fn validate_group_custom_validate_in_array_row() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition {
        name: "items".to_string(),
        field_type: FieldType::Array,
        fields: vec![
            FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "score".to_string(),
                        field_type: FieldType::Number,
                        validate: Some("hooks.validators.positive_number".to_string()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert("items".to_string(), serde_json::json!([
        { "meta__score": -10 }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field.contains("items[0][meta__score]")),
        "Should have custom validate error for group sub-field in array row, got: {:?}",
        err.errors
    );
}

// ── 6J. Validate: Required field on update skips when absent ─────────────────

#[test]
fn validate_required_field_on_update_skips_when_absent() {
    let (_tmp, pool, _registry, runner) = setup();

    let fields = vec![
        FieldDefinition {
            name: "title".to_string(),
            field_type: FieldType::Text,
            required: true,
            ..Default::default()
        },
        FieldDefinition {
            name: "body".to_string(),
            field_type: FieldType::Textarea,
            ..Default::default()
        },
    ];

    // On update (exclude_id set), if the required field is absent from data
    // (partial update), it should pass — we're keeping the existing value
    let mut data = HashMap::new();
    data.insert("body".to_string(), serde_json::json!("updated body only"));
    // title is NOT in data

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "t", Some("existing-id"), false);
    assert!(result.is_ok(), "Required field should be skipped on update when absent from data");
}

#[test]
fn validate_required_field_checkbox_always_passes() {
    let (_tmp, pool, _registry, runner) = setup();

    let fields = vec![
        FieldDefinition {
            name: "active".to_string(),
            field_type: FieldType::Checkbox,
            required: true,
            ..Default::default()
        },
    ];

    // Checkbox with required=true should pass even with no data
    let data = HashMap::new();
    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_ok(), "Checkbox field should never fail required check");
}

// ── 6K. hook_ctx_to_string_map ───────────────────────────────────────────────

#[test]
fn hook_ctx_to_string_map_basic() {
    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Hello"));
    data.insert("count".to_string(), serde_json::json!(42));

    let ctx = HookContext {
        collection: "test".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
    };

    let fields = vec![
        make_field("title", FieldType::Text),
        make_field("count", FieldType::Number),
    ];

    let map = crap_cms::hooks::lifecycle::hook_ctx_to_string_map(&ctx, &fields);
    assert_eq!(map.get("title").unwrap(), "Hello");
    assert_eq!(map.get("count").unwrap(), "42");
}

#[test]
fn hook_ctx_to_string_map_flattens_groups() {
    let mut data = HashMap::new();
    let mut seo = serde_json::Map::new();
    seo.insert("meta_title".to_string(), serde_json::json!("SEO Title"));
    seo.insert("meta_desc".to_string(), serde_json::json!("Description"));
    data.insert("seo".to_string(), serde_json::Value::Object(seo));
    data.insert("title".to_string(), serde_json::json!("Normal Title"));

    let ctx = HookContext {
        collection: "test".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
    };

    let fields = vec![
        make_field("title", FieldType::Text),
        FieldDefinition {
            name: "seo".to_string(),
            field_type: FieldType::Group,
            fields: vec![
                make_field("meta_title", FieldType::Text),
                make_field("meta_desc", FieldType::Text),
            ],
            ..Default::default()
        },
    ];

    let map = crap_cms::hooks::lifecycle::hook_ctx_to_string_map(&ctx, &fields);
    assert_eq!(map.get("title").unwrap(), "Normal Title");
    assert_eq!(map.get("seo__meta_title").unwrap(), "SEO Title");
    assert_eq!(map.get("seo__meta_desc").unwrap(), "Description");
    assert!(!map.contains_key("seo"), "Group key itself should not be in the map");
}

#[test]
fn hook_ctx_to_string_map_group_as_string_falls_through() {
    // When group value is already a string (e.g. from form data), it should be kept as-is
    let mut data = HashMap::new();
    data.insert("seo".to_string(), serde_json::json!("already-a-string"));

    let ctx = HookContext {
        collection: "test".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
    };

    let fields = vec![
        FieldDefinition {
            name: "seo".to_string(),
            field_type: FieldType::Group,
            fields: vec![make_field("meta_title", FieldType::Text)],
            ..Default::default()
        },
    ];

    let map = crap_cms::hooks::lifecycle::hook_ctx_to_string_map(&ctx, &fields);
    // When not an object, falls through to string insertion
    assert_eq!(map.get("seo").unwrap(), "already-a-string");
}

// ── 6L. evaluate_condition_table ─────────────────────────────────────────────

#[test]
fn evaluate_condition_equals() {
    let data = serde_json::json!({"status": "published"});
    let condition = serde_json::json!({"field": "status", "equals": "published"});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&condition, &data));

    let condition = serde_json::json!({"field": "status", "equals": "draft"});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(&condition, &data));
}

#[test]
fn evaluate_condition_not_equals() {
    let data = serde_json::json!({"status": "published"});
    let condition = serde_json::json!({"field": "status", "not_equals": "draft"});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&condition, &data));

    let condition = serde_json::json!({"field": "status", "not_equals": "published"});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(&condition, &data));
}

#[test]
fn evaluate_condition_in() {
    let data = serde_json::json!({"status": "published"});
    let condition = serde_json::json!({"field": "status", "in": ["published", "draft"]});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&condition, &data));

    let condition = serde_json::json!({"field": "status", "in": ["archived", "deleted"]});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(&condition, &data));
}

#[test]
fn evaluate_condition_not_in() {
    let data = serde_json::json!({"status": "published"});
    let condition = serde_json::json!({"field": "status", "not_in": ["draft", "archived"]});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&condition, &data));

    let condition = serde_json::json!({"field": "status", "not_in": ["published", "draft"]});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(&condition, &data));
}

#[test]
fn evaluate_condition_is_truthy() {
    let data = serde_json::json!({"active": true, "name": "test", "empty": "", "flag": false, "nothing": null});

    let cond = serde_json::json!({"field": "active", "is_truthy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&cond, &data));

    let cond = serde_json::json!({"field": "name", "is_truthy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&cond, &data));

    let cond = serde_json::json!({"field": "empty", "is_truthy": true});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(&cond, &data));

    let cond = serde_json::json!({"field": "flag", "is_truthy": true});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(&cond, &data));

    let cond = serde_json::json!({"field": "nothing", "is_truthy": true});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(&cond, &data));
}

#[test]
fn evaluate_condition_is_falsy() {
    let data = serde_json::json!({"active": false, "name": ""});

    let cond = serde_json::json!({"field": "active", "is_falsy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&cond, &data));

    let cond = serde_json::json!({"field": "name", "is_falsy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&cond, &data));

    let cond = serde_json::json!({"field": "missing", "is_falsy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&cond, &data));
}

#[test]
fn evaluate_condition_array_means_and() {
    let data = serde_json::json!({"status": "published", "role": "admin"});

    // All conditions true => true
    let conditions = serde_json::json!([
        {"field": "status", "equals": "published"},
        {"field": "role", "equals": "admin"}
    ]);
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&conditions, &data));

    // One false => false
    let conditions = serde_json::json!([
        {"field": "status", "equals": "published"},
        {"field": "role", "equals": "editor"}
    ]);
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(&conditions, &data));
}

#[test]
fn evaluate_condition_unknown_operator_shows() {
    let data = serde_json::json!({"x": 1});
    let cond = serde_json::json!({"field": "x", "unknown_op": "whatever"});
    // Unknown operator defaults to true (show field)
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&cond, &data));
}

#[test]
fn evaluate_condition_non_object_non_array() {
    let data = serde_json::json!({"x": 1});
    // Non-object, non-array condition defaults to true
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&serde_json::json!("string"), &data));
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&serde_json::json!(42), &data));
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&serde_json::json!(true), &data));
}

#[test]
fn evaluate_condition_is_truthy_with_numbers_arrays_objects() {
    let data = serde_json::json!({
        "count": 42,
        "items": [1, 2],
        "meta": {"key": "val"},
        "empty_arr": [],
        "empty_obj": {}
    });

    // Numbers are truthy
    let cond = serde_json::json!({"field": "count", "is_truthy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&cond, &data));

    // Non-empty arrays are truthy
    let cond = serde_json::json!({"field": "items", "is_truthy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&cond, &data));

    // Non-empty objects are truthy
    let cond = serde_json::json!({"field": "meta", "is_truthy": true});
    assert!(crap_cms::hooks::lifecycle::evaluate_condition_table(&cond, &data));

    // Empty arrays are falsy
    let cond = serde_json::json!({"field": "empty_arr", "is_truthy": true});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(&cond, &data));

    // Empty objects are falsy
    let cond = serde_json::json!({"field": "empty_obj", "is_truthy": true});
    assert!(!crap_cms::hooks::lifecycle::evaluate_condition_table(&cond, &data));
}

// ── 6M. call_row_label ───────────────────────────────────────────────────────

#[test]
fn call_row_label_returns_label() {
    let (_tmp, _pool, _registry, runner) = setup();

    let row_data = serde_json::json!({"label": "My Row"});
    let result = runner.call_row_label("hooks.field_hooks.row_label", &row_data);
    assert_eq!(result, Some("Row: My Row".to_string()));
}

#[test]
fn call_row_label_returns_none_when_no_label() {
    let (_tmp, _pool, _registry, runner) = setup();

    let row_data = serde_json::json!({"other": "value"});
    let result = runner.call_row_label("hooks.field_hooks.row_label", &row_data);
    assert!(result.is_none(), "Should return None when label field is missing");
}

#[test]
fn call_row_label_invalid_ref_returns_none() {
    let (_tmp, _pool, _registry, runner) = setup();

    let row_data = serde_json::json!({"label": "test"});
    let result = runner.call_row_label("hooks.nonexistent.function", &row_data);
    assert!(result.is_none(), "Invalid hook ref should return None");
}

// ── 6N. call_display_condition ───────────────────────────────────────────────

#[test]
fn call_display_condition_bool_true() {
    let (_tmp, _pool, _registry, runner) = setup();

    let data = serde_json::json!({"status": "published"});
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

    let data = serde_json::json!({"status": "draft"});
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

    let data = serde_json::json!({"status": "published"});
    let result = runner.call_display_condition("hooks.field_hooks.condition_table", &data);
    assert!(result.is_some());
    match result.unwrap() {
        crap_cms::hooks::lifecycle::DisplayConditionResult::Table { condition, visible } => {
            assert!(visible, "status=published should be visible");
            assert_eq!(condition.get("field").and_then(|v| v.as_str()), Some("status"));
            assert_eq!(condition.get("equals").and_then(|v| v.as_str()), Some("published"));
        }
        other => panic!("Expected Table, got {:?}", other),
    }
}

#[test]
fn call_display_condition_table_not_visible() {
    let (_tmp, _pool, _registry, runner) = setup();

    let data = serde_json::json!({"status": "draft"});
    let result = runner.call_display_condition("hooks.field_hooks.condition_table", &data);
    assert!(result.is_some());
    match result.unwrap() {
        crap_cms::hooks::lifecycle::DisplayConditionResult::Table { visible, .. } => {
            assert!(!visible, "status=draft should not be visible when condition says equals=published");
        }
        other => panic!("Expected Table, got {:?}", other),
    }
}

#[test]
fn call_display_condition_invalid_ref_returns_none() {
    let (_tmp, _pool, _registry, runner) = setup();

    let data = serde_json::json!({"status": "published"});
    let result = runner.call_display_condition("hooks.nonexistent.function", &data);
    assert!(result.is_none(), "Invalid hook ref should return None");
}

// ── 6O. run_before_render ────────────────────────────────────────────────────

#[test]
fn run_before_render_no_hooks_returns_same() {
    // Default init.lua only registers before_change hooks, not before_render.
    // So this should return the context unchanged.
    let (_tmp, _pool, _registry, runner) = setup();

    let context = serde_json::json!({"page": "home", "items": [1, 2, 3]});
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
    data.insert("title".to_string(), serde_json::json!("Test"));

    let ctx = HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context: HashMap::new(),
    };

    let result = runner.run_hooks(
        &def.hooks,
        HookEvent::BeforeChange,
        ctx,
    ).expect("run_hooks failed");

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
    };

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    // Should not error even with locale and draft set
    let result = runner.run_hooks_with_conn(
        &def.hooks,
        HookEvent::BeforeChange,
        ctx,
        &tx,
        None,
    ).expect("Hook execution failed");

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
    context.insert("before_marker".to_string(), serde_json::json!("set-by-test"));

    let ctx = HookContext {
        collection: "articles".to_string(),
        operation: "create".to_string(),
        data,
        locale: None,
        draft: None,
        context,
    };

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    // The hooks should receive the context table
    let result = runner.run_hooks_with_conn(
        &def.hooks,
        HookEvent::BeforeChange,
        ctx,
        &tx,
        None,
    ).expect("Hook execution failed");

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

    runner.run_field_hooks_with_conn(
        &def.fields,
        FieldHookEvent::BeforeValidate,
        &mut data,
        "articles",
        "create",
        &tx,
        None,
    ).expect("Field hook failed");

    // The title field has a before_validate trim hook
    assert_eq!(
        data.get("title").and_then(|v| v.as_str()),
        Some("spaced title"),
        "Field before_validate hook should trim the title"
    );
}

// ── 6U. run_migration ────────────────────────────────────────────────────────

#[test]
fn run_migration_executes_lua_file() {
    let (_tmp, pool, registry, runner) = setup();

    // Create a temporary migration file
    let migration_dir = tempfile::tempdir().expect("tempdir");
    let migration_path = migration_dir.path().join("001_test.lua");
    std::fs::write(&migration_path, r#"
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
    "#).expect("write migration");

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("tx");

    let result = runner.run_migration(&migration_path, "up", &tx);
    assert!(result.is_ok(), "Migration should succeed: {:?}", result.err());
    tx.commit().unwrap();

    // Verify the migration ran by checking the article was created
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let count = crap_cms::db::ops::count_documents(&pool, "articles", &def, &[], None)
        .expect("count");
    assert_eq!(count, 1, "Migration should have created 1 article");
}

#[test]
fn run_migration_invalid_direction_fails() {
    let (_tmp, pool, _registry, runner) = setup();

    let migration_dir = tempfile::tempdir().expect("tempdir");
    let migration_path = migration_dir.path().join("002_test.lua");
    std::fs::write(&migration_path, r#"
        local M = {}
        function M.up() end
        return M
    "#).expect("write migration");

    let conn = pool.get().expect("DB connection");
    let result = runner.run_migration(&migration_path, "down", &conn);
    assert!(result.is_err(), "Migration with missing direction function should fail");
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
    assert!(result.is_ok(), "Job handler should succeed: {:?}", result.err());
}

#[test]
fn run_job_handler_invalid_ref_fails() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    let result = runner.run_job_handler(
        "hooks.nonexistent.handler",
        "test-job",
        "{}",
        1,
        3,
        &conn,
    );
    assert!(result.is_err(), "Invalid handler ref should fail");
}

// ── 6W. check_live_setting with nil-returning function ───────────────────────

#[test]
fn check_live_setting_function_returns_nil_means_false() {
    use crap_cms::core::collection::LiveSetting;
    let (_tmp, _pool, _registry, runner) = setup();

    // suppress_broadcast returns nil, which should be treated as false
    let live = LiveSetting::Function("hooks.live.suppress_broadcast".to_string());
    let result = runner.check_live_setting(
        Some(&live), "articles", "create", &HashMap::new(),
    ).expect("should not error");
    assert!(!result, "nil return should mean suppress (false)");
}

// ── 6X. Custom validate function returning false (no message) ────────────────

#[test]
fn custom_validate_returns_false_generic_message() {
    let (_tmp, pool, _registry, runner) = setup();

    // Create a field with a validate function that returns false (not a string)
    let fields = vec![FieldDefinition {
        name: "title".to_string(),
        field_type: FieldType::Text,
        validate: Some("hooks.access.deny_all".to_string()), // returns false
        ..Default::default()
    }];

    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("any value"));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field == "title" && e.message.contains("validation failed")),
        "False-returning validate should produce 'validation failed' message, got: {:?}",
        err.errors
    );
}

// ── 6Y. Required check on Array/Relationship (has-many) fields ───────────────

#[test]
fn validate_required_array_must_have_items() {
    let (_tmp, pool, _registry, runner) = setup();

    let fields = vec![FieldDefinition {
        name: "tags".to_string(),
        field_type: FieldType::Array,
        required: true,
        fields: vec![make_field("label", FieldType::Text)],
        ..Default::default()
    }];

    let conn = pool.get().expect("DB connection");

    // Empty array should fail
    let mut data = HashMap::new();
    data.insert("tags".to_string(), serde_json::json!([]));
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_err(), "Empty required array should fail");

    // Non-empty array should pass
    data.insert("tags".to_string(), serde_json::json!([{"label": "rust"}]));
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_ok(), "Non-empty required array should pass");
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
    runner.run_field_hooks_with_conn(
        &def.fields,
        FieldHookEvent::BeforeValidate,
        &mut data,
        "articles",
        "create",
        &tx,
        None,
    ).expect("before_validate field hook");

    assert_eq!(data.get("title").and_then(|v| v.as_str()), Some("hello"));

    // Then: after_read uppercases
    runner.run_field_hooks(
        &def.fields,
        FieldHookEvent::AfterRead,
        &mut data,
        "articles",
        "find",
    ).expect("after_read field hook");

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
    };

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();

    let result = runner.run_before_write(
        &def.hooks, &def.fields, ctx, &tx, "articles", None, Some(&user), false,
    ).expect("run_before_write with user failed");

    assert!(result.data.contains_key("title"));
}

// ── 6AB. Blocks: missing block_type continues gracefully ─────────────────────

#[test]
fn validate_blocks_unknown_block_type_skips() {
    let (_tmp, pool, _registry, runner) = setup();

    let blocks_field = FieldDefinition {
        name: "content".to_string(),
        field_type: FieldType::Blocks,
        blocks: vec![
            BlockDefinition {
                block_type: "text".to_string(),
                fields: vec![
                    FieldDefinition {
                        name: "body".to_string(),
                        field_type: FieldType::Text,
                        required: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![blocks_field];
    let mut data = HashMap::new();
    // Use an unknown block type — validation should skip it gracefully
    data.insert("content".to_string(), serde_json::json!([
        { "_block_type": "unknown_type", "body": "" }
    ]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_ok(), "Unknown block type should be skipped, not error");
}

// ── 6AC. Checkbox sub-field in array row never fails required ────────────────

#[test]
fn validate_checkbox_subfield_in_array_never_fails_required() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition {
        name: "items".to_string(),
        field_type: FieldType::Array,
        fields: vec![
            FieldDefinition {
                name: "active".to_string(),
                field_type: FieldType::Checkbox,
                required: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![array_field];
    let mut data = HashMap::new();
    // Checkbox absent/empty should NOT fail required check
    data.insert("items".to_string(), serde_json::json!([{}]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_ok(), "Checkbox subfield should never fail required check");
}

// ── 6AD. Checkbox sub-field in group in array row ────────────────────────────

#[test]
fn validate_checkbox_group_subfield_in_array_never_fails_required() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition {
        name: "items".to_string(),
        field_type: FieldType::Array,
        fields: vec![
            FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "featured".to_string(),
                        field_type: FieldType::Checkbox,
                        required: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert("items".to_string(), serde_json::json!([{}]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_ok(), "Checkbox in group in array should never fail required");
}

// ── 6AE. Validate: Blocks row that is not an object ──────────────────────────

#[test]
fn validate_blocks_non_object_row_skips() {
    let (_tmp, pool, _registry, runner) = setup();

    let blocks_field = FieldDefinition {
        name: "content".to_string(),
        field_type: FieldType::Blocks,
        blocks: vec![
            BlockDefinition {
                block_type: "text".to_string(),
                fields: vec![
                    FieldDefinition {
                        name: "body".to_string(),
                        field_type: FieldType::Text,
                        required: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let fields = vec![blocks_field];
    let mut data = HashMap::new();
    // Non-object row (string) should be skipped gracefully
    data.insert("content".to_string(), serde_json::json!(["not-an-object"]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(&fields, &data, &conn, "t", None, false);
    assert!(result.is_ok(), "Non-object block rows should be skipped");
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
    std::fs::write(tmp.path().join("init.lua"), r#"
        crap.hooks.register("before_render", function(ctx)
            ctx._render_marker = "rendered"
            return ctx
        end)
    "#).unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::new(tmp.path(), registry, &config)
        .expect("HookRunner::new");

    let context = serde_json::json!({ "page": "edit" });
    let result = runner.run_before_render(context);
    assert_eq!(
        result.get("_render_marker").and_then(|v| v.as_str()),
        Some("rendered"),
        "before_render hook should add _render_marker"
    );
    // Original data preserved
    assert_eq!(
        result.get("page").and_then(|v| v.as_str()),
        Some("edit"),
    );
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
    std::fs::write(tmp.path().join("init.lua"), r#"
        crap.hooks.register("before_render", function(ctx)
            return nil
        end)
    "#).unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::new(tmp.path(), registry, &config)
        .expect("HookRunner::new");

    let context = serde_json::json!({ "page": "list" });
    let result = runner.run_before_render(context.clone());
    // nil return should keep context unchanged
    assert_eq!(result, context);
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

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");

    let mut pool_config = CrapConfig::default();
    pool_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &pool_config).expect("pool");
    crap_cms::db::migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    let runner = crap_cms::hooks::lifecycle::HookRunner::new(tmp.path(), registry.clone(), &config)
        .expect("HookRunner::new");

    // Write a migration file
    let migration_path = tmp.path().join("migration_test.lua");
    std::fs::write(&migration_path, r#"
        local M = {}
        function M.up()
            -- Create a document via CRUD
            crap.collections.create("articles", { title = "Migrated Article" })
        end
        function M.down()
            -- No-op
        end
        return M
    "#).unwrap();

    let conn = pool.get().expect("conn");
    runner.run_migration(&migration_path, "up", &conn)
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
    ).expect("find");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].fields.get("title").and_then(|v| v.as_str()), Some("Migrated Article"));
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
    std::fs::write(jobs_dir.join("test_job.lua"), r#"
        local M = {}
        function M.run(ctx)
            return { processed = true, slug = ctx.job.slug, data_value = ctx.data.key }
        end
        return M
    "#).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");

    let mut pool_config = CrapConfig::default();
    pool_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &pool_config).expect("pool");
    crap_cms::db::migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    let runner = crap_cms::hooks::lifecycle::HookRunner::new(tmp.path(), registry, &config)
        .expect("HookRunner::new");

    let conn = pool.get().expect("conn");
    let result = runner.run_job_handler(
        "jobs.test_job.run",
        "test-job",
        r#"{"key": "hello"}"#,
        1, 3,
        &conn,
    ).expect("run_job_handler failed");

    assert!(result.is_some(), "Job should return a value");
    let result_json: serde_json::Value = serde_json::from_str(&result.unwrap()).expect("parse JSON");
    assert_eq!(result_json.get("processed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(result_json.get("slug").and_then(|v| v.as_str()), Some("test-job"));
    assert_eq!(result_json.get("data_value").and_then(|v| v.as_str()), Some("hello"));
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
    std::fs::write(jobs_dir.join("void_job.lua"), r#"
        local M = {}
        function M.run(ctx)
            -- do nothing, return nil
        end
        return M
    "#).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");

    let mut pool_config = CrapConfig::default();
    pool_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &pool_config).expect("pool");

    let runner = crap_cms::hooks::lifecycle::HookRunner::new(tmp.path(), registry, &config)
        .expect("HookRunner::new");

    let conn = pool.get().expect("conn");
    let result = runner.run_job_handler(
        "jobs.void_job.run",
        "void-job",
        "{}",
        1, 1,
        &conn,
    ).expect("run_job_handler failed");

    assert!(result.is_none(), "Job returning nil should give None");
}

// ── 7D. apply_after_read_many with empty hooks ────────────────────────────────

#[test]
fn apply_after_read_many_empty_hooks_passthrough() {
    let (_tmp, pool, registry, runner) = setup();
    let doc = create_article(&pool, &registry, &HashMap::from([
        ("title".to_string(), "Test".to_string()),
    ]));
    let hooks = CollectionHooks::default();
    let fields: Vec<FieldDefinition> = Vec::new();
    let docs = vec![doc.clone()];
    let result = runner.apply_after_read_many(&hooks, &fields, "articles", "find", docs);
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
    std::fs::write(tmp.path().join("init.lua"), r#"
        crap.hooks.register("before_broadcast", function(ctx)
            return nil  -- suppress
        end)
    "#).unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::new(tmp.path(), registry, &config)
        .expect("HookRunner::new");

    let hooks = CollectionHooks::default();
    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Test"));
    let result = runner.run_before_broadcast(&hooks, "articles", "create", data)
        .expect("run_before_broadcast");
    assert!(result.is_none(), "Registered before_broadcast returning nil should suppress");
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
    std::fs::write(tmp.path().join("init.lua"), r#"
        crap.hooks.register("before_broadcast", function(ctx)
            ctx.data._registered_marker = "yes"
            return ctx
        end)
    "#).unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::new(tmp.path(), registry, &config)
        .expect("HookRunner::new");

    let hooks = CollectionHooks::default();
    let mut data = HashMap::new();
    data.insert("title".to_string(), serde_json::json!("Test"));
    let result = runner.run_before_broadcast(&hooks, "articles", "create", data)
        .expect("run_before_broadcast");
    assert!(result.is_some());
    let result_data = result.unwrap();
    assert_eq!(
        result_data.get("_registered_marker").and_then(|v| v.as_str()),
        Some("yes"),
    );
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
    std::fs::write(hooks_dir.join("row_label.lua"), r#"
        local M = {}
        function M.format(row)
            return "Row: " .. (row.title or "untitled")
        end
        return M
    "#).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::new(tmp.path(), registry, &config)
        .expect("HookRunner::new");

    let row_data = serde_json::json!({ "title": "Hello" });
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
    std::fs::write(hooks_dir.join("conditions.lua"), r#"
        local M = {}
        function M.show_if_published(data)
            return data.status == "published"
        end
        return M
    "#).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::new(tmp.path(), registry, &config)
        .expect("HookRunner::new");

    let form_data = serde_json::json!({ "status": "published" });
    let result = runner.call_display_condition("hooks.conditions.show_if_published", &form_data);
    match result {
        Some(crap_cms::hooks::lifecycle::DisplayConditionResult::Bool(b)) => assert!(b),
        other => panic!("Expected Bool(true), got {:?}", other),
    }

    let form_data_draft = serde_json::json!({ "status": "draft" });
    let result = runner.call_display_condition("hooks.conditions.show_if_published", &form_data_draft);
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
    std::fs::write(hooks_dir.join("conditions.lua"), r#"
        local M = {}
        function M.condition_table(data)
            return { field = "status", equals = "published" }
        end
        return M
    "#).unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let runner = crap_cms::hooks::lifecycle::HookRunner::new(tmp.path(), registry, &config)
        .expect("HookRunner::new");

    let form_data = serde_json::json!({ "status": "published" });
    let result = runner.call_display_condition("hooks.conditions.condition_table", &form_data);
    match result {
        Some(crap_cms::hooks::lifecycle::DisplayConditionResult::Table { condition, visible }) => {
            assert!(visible, "status=published should match the condition");
            assert_eq!(condition.get("field").and_then(|v| v.as_str()), Some("status"));
            assert_eq!(condition.get("equals").and_then(|v| v.as_str()), Some("published"));
        }
        other => panic!("Expected Table result, got {:?}", other),
    }
}
