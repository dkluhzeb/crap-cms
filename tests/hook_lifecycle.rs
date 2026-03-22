use std::collections::HashMap;
use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::Document;
use crap_cms::core::collection::Hooks;
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::db::query::AccessResult;
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::{AfterReadCtx, FieldWriteCtx, HookRunner, ValidationCtx};
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
fn before_change_hook_modifies_data() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), json!("  Test Title  "));
    data.insert("body".to_string(), json!("Content"));

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

    // run_before_write runs validate → hooks
    // But let's test run_hooks_with_conn for before_change directly
    let result = runner
        .run_hooks_with_conn(
            &def.hooks,
            crap_cms::hooks::lifecycle::HookEvent::BeforeChange,
            ctx,
            &tx,
        )
        .expect("Hook execution failed");

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
    data.insert("title".to_string(), json!("  Spaces Around  "));

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

    let result = runner
        .run_hooks_with_conn(
            &def.hooks,
            crap_cms::hooks::lifecycle::HookEvent::BeforeValidate,
            ctx,
            &tx,
        )
        .expect("Hook execution failed");

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
    data.insert("title".to_string(), json!("Hello World"));
    // slug intentionally left empty — field hook should auto-generate from title

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    runner
        .run_field_hooks_with_conn(
            &def.fields,
            crap_cms::hooks::lifecycle::FieldHookEvent::BeforeChange,
            &mut data,
            "articles",
            "create",
            &FieldWriteCtx::builder(&tx).build(),
        )
        .expect("Field hook failed");

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
    data.insert("title".to_string(), json!("Test"));

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

    let result = runner
        .run_hooks_with_conn(
            &def.hooks,
            crap_cms::hooks::lifecycle::HookEvent::BeforeChange,
            ctx,
            &tx,
        )
        .expect("Hook execution failed");

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
    let result = runner.eval_lua_with_conn(r#"error("intentional hook error")"#, &conn, None);
    assert!(result.is_err(), "Lua error should propagate as Rust error");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("intentional hook error")
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
    data.insert("title".to_string(), json!("  My Article  "));
    data.insert("body".to_string(), json!("Article body"));

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

    let result = runner
        .run_before_write(
            &def.hooks,
            &def.fields,
            ctx,
            &ValidationCtx::builder(&tx, "articles").build(),
        )
        .expect("run_before_write failed");

    // Title should be trimmed (before_validate hook)
    assert_eq!(
        result.data.get("title").and_then(|v| v.as_str()),
        Some("My Article")
    );
    // before_change hook marker
    assert_eq!(
        result.data.get("_hook_ran").and_then(|v| v.as_str()),
        Some("before_change")
    );
    // global hook marker
    assert_eq!(
        result.data.get("_global_hook_ran").and_then(|v| v.as_str()),
        Some("true")
    );
    // slug should have been generated by field hook
    let slug = result.data.get("slug").and_then(|v| v.as_str());
    assert!(
        slug.is_some(),
        "slug should have been generated by field hook"
    );
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
        user: None,
        ui_locale: None,
    };

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    let result = runner.run_before_write(
        &def.hooks,
        &def.fields,
        ctx,
        &ValidationCtx::builder(&tx, "articles").build(),
    );
    assert!(
        result.is_err(),
        "run_before_write should fail when validation fails"
    );
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
    let result = runner
        .eval_lua_with_conn(
            r#"
        local r = crap.collections.find("articles", { overrideAccess = true })
        return tostring(r.pagination.totalDocs)
        "#,
            &conn,
            None,
        )
        .expect("Eval failed");
    assert_eq!(result, "1", "Should find 1 article");
}

// ── Helper: build a minimal FieldDefinition ──────────────────────────────────

fn make_field(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, field_type).build()
}

fn make_field_with_read_access(name: &str, read_ref: &str) -> FieldDefinition {
    let mut f = make_field(name, FieldType::Text);
    f.access.read = Some(read_ref.to_string());
    f
}

fn make_field_with_write_access(
    name: &str,
    create_ref: Option<&str>,
    update_ref: Option<&str>,
) -> FieldDefinition {
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

    let result = runner
        .check_access(None, None, None, None, &conn)
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

    let result = runner
        .check_access(Some("hooks.access.allow_all"), None, None, None, &conn)
        .expect("check_access failed");
    assert!(
        matches!(result, AccessResult::Allowed),
        "allow_all should return Allowed"
    );
}

#[test]
fn check_access_returns_denied() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    let result = runner
        .check_access(Some("hooks.access.deny_all"), None, None, None, &conn)
        .expect("check_access failed");
    assert!(
        matches!(result, AccessResult::Denied),
        "deny_all should return Denied"
    );
}

#[test]
fn check_access_returns_constrained() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().expect("DB connection");

    let result = runner
        .check_access(Some("hooks.access.constrained"), None, None, None, &conn)
        .expect("check_access failed");
    match result {
        AccessResult::Constrained(clauses) => {
            assert!(
                !clauses.is_empty(),
                "Constrained should have at least one clause"
            );
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
    admin_fields.insert("role".to_string(), json!("admin"));
    let admin_user = Document {
        id: "user-1".into(),
        fields: admin_fields,
        created_at: None,
        updated_at: None,
    };

    let result = runner
        .check_access(
            Some("hooks.access.check_role"),
            Some(&admin_user),
            None,
            None,
            &conn,
        )
        .expect("check_access failed");
    assert!(
        matches!(result, AccessResult::Allowed),
        "Admin user should be Allowed by check_role"
    );

    // User without admin role should be denied
    let mut regular_fields = HashMap::new();
    regular_fields.insert("role".to_string(), json!("editor"));
    let regular_user = Document {
        id: "user-2".into(),
        fields: regular_fields,
        created_at: None,
        updated_at: None,
    };

    let result = runner
        .check_access(
            Some("hooks.access.check_role"),
            Some(&regular_user),
            None,
            None,
            &conn,
        )
        .expect("check_access failed");
    assert!(
        matches!(result, AccessResult::Denied),
        "Non-admin user should be Denied by check_role"
    );

    // No user at all should be denied
    let result = runner
        .check_access(Some("hooks.access.check_role"), None, None, None, &conn)
        .expect("check_access failed");
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
    assert!(
        denied.is_empty(),
        "No create restriction defined, should be empty"
    );
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
    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: "articles",
        operation: "find",
        user: None,
        ui_locale: None,
    };
    let transformed = runner.apply_after_read(&ar_ctx, doc.clone());

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
    let ar_ctx = AfterReadCtx {
        hooks: &def.hooks,
        fields: &def.fields,
        collection: "articles",
        operation: "find",
        user: None,
        ui_locale: None,
    };
    let transformed = runner.apply_after_read_many(&ar_ctx, docs);

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
    let empty_hooks = Hooks::default();
    let empty_fields: Vec<FieldDefinition> = Vec::new();

    let original_id = doc.id.clone();
    let original_title = doc.fields.get("title").cloned();

    let ar_ctx = AfterReadCtx {
        hooks: &empty_hooks,
        fields: &empty_fields,
        collection: "articles",
        operation: "find",
        user: None,
        ui_locale: None,
    };
    let result = runner.apply_after_read(&ar_ctx, doc);

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
    let result = runner
        .run_auth_strategy(
            "hooks.auth_strategy.api_key_auth",
            "articles",
            &headers,
            &conn,
        )
        .expect("should not error");
    assert!(result.is_none(), "Invalid key should return None");
}

#[test]
fn auth_strategy_returns_none_on_missing_header() {
    let (_tmp, pool, _registry, runner) = setup();

    let headers = HashMap::new(); // no x-api-key header

    let conn = pool.get().expect("DB connection");
    let result = runner
        .run_auth_strategy(
            "hooks.auth_strategy.api_key_auth",
            "articles",
            &headers,
            &conn,
        )
        .expect("should not error");
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
    let result = runner
        .run_auth_strategy(
            "hooks.auth_strategy.api_key_auth",
            "articles",
            &headers,
            &conn,
        )
        .expect("should not error");
    assert!(
        result.is_some(),
        "Strategy with CRUD access should find articles and return one"
    );
}

// ── 4B. Live Events ──────────────────────────────────────────────────────────

#[test]
fn check_live_setting_none_allows() {
    let (_tmp, _pool, _registry, runner) = setup();
    let result = runner.check_live_setting(None, "articles", "create", &HashMap::new());
    assert!(result.is_ok());
    assert!(result.unwrap(), "None live setting should allow broadcast");
}
