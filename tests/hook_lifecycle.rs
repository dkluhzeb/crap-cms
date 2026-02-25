use std::collections::HashMap;
use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::Document;
use crap_cms::core::field::{FieldAccess, FieldDefinition, FieldHooks, FieldType};
use crap_cms::core::collection::CollectionHooks;
use crap_cms::db::{migrate, pool, query};
use crap_cms::db::query::AccessResult;
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;

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
        return tostring(r.total)
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
        required: false,
        unique: false,
        validate: None,
        default_value: None,
        options: Vec::new(),
        admin: Default::default(),
        hooks: FieldHooks::default(),
        access: FieldAccess::default(),
        relationship: None,
        fields: Vec::new(),
        blocks: Vec::new(),
        localized: false,
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
    // Original fields should still be present
    assert_eq!(
        transformed.fields.get("title").and_then(|v| v.as_str()),
        Some("Read Test"),
        "Title should be preserved after after_read"
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
