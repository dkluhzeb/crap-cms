use std::collections::HashMap;
use std::path::PathBuf;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::Document;
use crap_cms::db::{DbConnection, FindQuery};
use crap_cms::db::{DbValue, migrate, ops, pool, query};
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{
    GetGlobalInput, ListVersionsInput, RunnerReadHooks, RunnerWriteHooks, SearchDocumentsInput,
    ServiceContext, WriteInput, get_global_document,
    jobs::{QueueJobInput, queue_job},
    list_versions, restore_collection_version, search_documents, update_global_core,
};
use serde_json::json;

fn setup() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    crap_cms::core::SharedRegistry,
    HookRunner,
) {
    let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("example");
    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(&config_dir, &config).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();

    // Sync schema so tables exist
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry.clone())
        .config(&config)
        .build()
        .unwrap();
    (tmp, db_pool, registry, runner)
}

fn make_user_doc(id: &str, role: &str) -> Document {
    let mut doc = Document::new(id.to_string());
    doc.fields.insert("role".into(), json!(role));
    doc.fields
        .insert("email".into(), json!(format!("{}@test.com", role)));
    doc.fields.insert("name".into(), json!(role.to_uppercase()));
    doc
}

// ── 1. Lua Parsing ──────────────────────────────────────────────────────────

#[test]
fn access_config_parsed_from_lua() {
    let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("example");
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).unwrap();
    let reg = registry.read().unwrap();
    let posts = reg
        .get_collection("posts")
        .expect("posts collection not found");

    assert_eq!(
        posts.access.read.as_deref(),
        Some("access.published_or_author")
    );
    assert_eq!(posts.access.create.as_deref(), Some("access.authenticated"));
    assert_eq!(
        posts.access.update.as_deref(),
        Some("access.author_or_editor")
    );
    assert_eq!(
        posts.access.trash.as_deref(),
        Some("access.editor_or_above")
    );
    assert_eq!(
        posts.access.delete.as_deref(),
        Some("access.admin_or_director")
    );
}

#[test]
fn field_access_parsed_from_lua() {
    let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("example");
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).unwrap();
    let reg = registry.read().unwrap();
    let posts = reg
        .get_collection("posts")
        .expect("posts collection not found");

    // New posts definition has no field-level access controls
    for field in &posts.fields {
        assert!(
            field.access.read.is_none(),
            "field {} has unexpected read access",
            field.name
        );
        assert!(
            field.access.create.is_none(),
            "field {} has unexpected create access",
            field.name
        );
        assert!(
            field.access.update.is_none(),
            "field {} has unexpected update access",
            field.name
        );
    }
}

// ── 2. Collection-Level check_access ────────────────────────────────────────

#[test]
fn no_access_ref_allows() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();

    let result = runner.check_access(None, None, None, None, &conn).unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));
}

#[test]
fn anyone_allows_anonymous() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();

    let result = runner
        .check_access(Some("access.anyone"), None, None, None, &conn)
        .unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));
}

#[test]
fn authenticated_denies_anonymous() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();

    let result = runner
        .check_access(Some("access.authenticated"), None, None, None, &conn)
        .unwrap();
    assert!(matches!(result, query::AccessResult::Denied));
}

#[test]
fn authenticated_allows_user() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();
    let editor = make_user_doc("editor-1", "editor");

    let result = runner
        .check_access(
            Some("access.authenticated"),
            Some(&editor),
            None,
            None,
            &conn,
        )
        .unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));
}

#[test]
fn admin_only_denies_editor() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();
    let editor = make_user_doc("editor-1", "editor");

    let result = runner
        .check_access(Some("access.admin_only"), Some(&editor), None, None, &conn)
        .unwrap();
    assert!(matches!(result, query::AccessResult::Denied));
}

#[test]
fn admin_only_allows_admin() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();
    let admin = make_user_doc("admin-1", "admin");

    let result = runner
        .check_access(Some("access.admin_only"), Some(&admin), None, None, &conn)
        .unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));
}

#[test]
fn published_or_author_constrains_anonymous() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();

    let result = runner
        .check_access(Some("access.published_or_author"), None, None, None, &conn)
        .unwrap();

    match result {
        query::AccessResult::Constrained(clauses) => {
            assert_eq!(clauses.len(), 1);
            match &clauses[0] {
                query::FilterClause::Single(f) => {
                    assert_eq!(f.field, "_status");
                    match &f.op {
                        query::FilterOp::Equals(val) => assert_eq!(val, "published"),
                        other => panic!("Expected Equals op, got {:?}", other),
                    }
                }
                other => panic!("Expected Single clause, got {:?}", other),
            }
        }
        other => panic!("Expected Constrained, got {:?}", other),
    }
}

#[test]
fn published_or_author_allows_admin() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();
    let admin = make_user_doc("admin-1", "admin");

    let result = runner
        .check_access(
            Some("access.published_or_author"),
            Some(&admin),
            None,
            None,
            &conn,
        )
        .unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));
}

// ── 3. Field-Level Access ───────────────────────────────────────────────────

#[test]
fn field_write_no_field_access_allows_all() {
    let (_tmp, pool, registry, runner) = setup();
    let conn = pool.get().unwrap();
    let editor = make_user_doc("editor-1", "editor");

    let reg = registry.read().unwrap();
    let posts = reg.get_collection("posts").unwrap();

    let denied = runner.check_field_write_access(&posts.fields, Some(&editor), "update", &conn);
    // No field-level access controls in posts definition
    assert!(
        denied.is_empty(),
        "Expected no denied fields, got: {:?}",
        denied
    );
}

#[test]
fn field_read_no_config_allows_all() {
    let (_tmp, pool, registry, runner) = setup();
    let conn = pool.get().unwrap();

    let reg = registry.read().unwrap();
    let posts = reg.get_collection("posts").unwrap();

    // No field has read access configured, so nothing should be denied
    let denied = runner.check_field_read_access(&posts.fields, None, &conn);
    assert!(
        denied.is_empty(),
        "Expected no denied fields for read, got: {:?}",
        denied
    );
}

// ── 4. End-to-End with DB ───────────────────────────────────────────────────

#[test]
fn constrained_find_filters_results() {
    let (_tmp, pool, registry, _runner) = setup();

    let reg = registry.read().unwrap();
    let posts = reg.get_collection("posts").unwrap().clone();
    drop(reg);

    // Create posts with different _status values via the versioning system
    let post_data = vec![
        ("Alpha Post", "alpha-post", "draft"),
        ("Beta Post", "beta-post", "published"),
        ("Gamma Post", "gamma-post", "draft"),
        ("Delta Post", "delta-post", "published"),
    ];
    for (title, slug, status) in &post_data {
        let mut data = HashMap::new();
        data.insert("title".to_string(), title.to_string());
        data.insert("slug".to_string(), slug.to_string());
        data.insert("excerpt".to_string(), "Test excerpt".to_string());

        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "posts", &posts, &data, None).unwrap();
        query::set_document_status(&tx, "posts", &doc.id, status).unwrap();
        tx.commit().unwrap();
    }

    // Simulate a Constrained access result (like published_or_author returns
    // for anonymous users), filtering to only published posts via _status.
    let constraint_filters = vec![query::FilterClause::Single(query::Filter {
        field: "_status".to_string(),
        op: query::FilterOp::Equals("published".to_string()),
    })];

    let find_query = query::FindQuery::builder()
        .filters(constraint_filters)
        .build();

    let docs = ops::find_documents(&pool, "posts", &posts, &find_query, None).unwrap();

    // Should only see published posts
    assert_eq!(
        docs.len(),
        2,
        "Expected 2 published posts, got {}",
        docs.len()
    );
}

#[test]
fn access_check_plus_db_query_end_to_end() {
    let (_tmp, pool, registry, runner) = setup();

    let reg = registry.read().unwrap();
    let posts = reg.get_collection("posts").unwrap().clone();
    drop(reg);

    // Create some posts
    for (i, slug) in ["e2e-post-1", "e2e-post-2"].iter().enumerate() {
        let mut data = HashMap::new();
        data.insert("title".to_string(), format!("E2E Post {}", i + 1));
        data.insert("slug".to_string(), slug.to_string());
        data.insert("excerpt".to_string(), "Test excerpt".to_string());

        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "posts", &posts, &data, None).unwrap();
        query::set_document_status(&tx, "posts", &doc.id, "published").unwrap();
        tx.commit().unwrap();
    }

    // Verify published_or_author for anonymous returns constraint (not fully open)
    let conn = pool.get().unwrap();
    let result = runner
        .check_access(posts.access.read.as_deref(), None, None, None, &conn)
        .unwrap();
    assert!(matches!(result, query::AccessResult::Constrained(_)));

    // Verify admin gets full access
    let admin = make_user_doc("admin-1", "admin");
    let result = runner
        .check_access(
            posts.access.read.as_deref(),
            Some(&admin),
            None,
            None,
            &conn,
        )
        .unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));

    let all_docs =
        ops::find_documents(&pool, "posts", &posts, &query::FindQuery::default(), None).unwrap();
    assert_eq!(all_docs.len(), 2);

    // Verify author_or_admin denies anonymous delete
    let result = runner
        .check_access(posts.access.delete.as_deref(), None, None, None, &conn)
        .unwrap();
    assert!(matches!(result, query::AccessResult::Denied));
}

// ── 5. Additional Access Control Tests ───────────────────────────────────────

#[test]
fn field_read_access_strips_denied_fields() {
    // Test check_field_read_access() with a field that has a deny_all read access
    // function. The denied field name should appear in the returned list.
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();

    // Build fields: one normal, one with read access that always denies, one normal
    let fields = vec![
        crap_cms::core::FieldDefinition {
            name: "title".to_string(),
            field_type: crap_cms::core::field::FieldType::Text,
            ..Default::default()
        },
        crap_cms::core::FieldDefinition {
            name: "secret_notes".to_string(),
            field_type: crap_cms::core::field::FieldType::Textarea,
            access: crap_cms::core::field::FieldAccess {
                read: Some("access.admin_only".to_string()),
                ..Default::default()
            },
            ..Default::default()
        },
        crap_cms::core::FieldDefinition {
            name: "body".to_string(),
            field_type: crap_cms::core::field::FieldType::Textarea,
            ..Default::default()
        },
    ];

    // Anonymous user (no user doc) — admin_only should deny
    let denied = runner.check_field_read_access(&fields, None, &conn);
    assert_eq!(
        denied.len(),
        1,
        "Should deny exactly one field for anonymous user"
    );
    assert_eq!(
        denied[0], "secret_notes",
        "The denied field should be 'secret_notes'"
    );

    // Admin user — admin_only should allow
    let admin = make_user_doc("admin-1", "admin");
    let denied = runner.check_field_read_access(&fields, Some(&admin), &conn);
    assert!(
        denied.is_empty(),
        "Admin user should have all fields allowed, but got denied: {:?}",
        denied
    );
}

#[test]
fn field_write_access_strips_denied_fields() {
    // Test check_field_write_access() for create vs update operations.
    // A field with a create-deny should be denied on create but not on update,
    // and vice versa.
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();

    let fields = vec![
        crap_cms::core::FieldDefinition {
            name: "title".to_string(),
            field_type: crap_cms::core::field::FieldType::Text,
            ..Default::default()
        },
        // This field denies create but allows update
        crap_cms::core::FieldDefinition {
            name: "auto_slug".to_string(),
            field_type: crap_cms::core::field::FieldType::Text,
            access: crap_cms::core::field::FieldAccess {
                create: Some("access.admin_only".to_string()),
                update: None,
                ..Default::default()
            },
            ..Default::default()
        },
        // This field allows create but denies update
        crap_cms::core::FieldDefinition {
            name: "immutable_field".to_string(),
            field_type: crap_cms::core::field::FieldType::Text,
            access: crap_cms::core::field::FieldAccess {
                create: None,
                update: Some("access.admin_only".to_string()),
                ..Default::default()
            },
            ..Default::default()
        },
    ];

    // Anonymous user: on create, auto_slug should be denied (admin_only denies anonymous)
    let denied_on_create = runner.check_field_write_access(&fields, None, "create", &conn);
    assert!(
        denied_on_create.contains(&"auto_slug".to_string()),
        "auto_slug should be denied on create for anonymous, got: {:?}",
        denied_on_create
    );
    assert!(
        !denied_on_create.contains(&"immutable_field".to_string()),
        "immutable_field should be allowed on create (no create access config), got: {:?}",
        denied_on_create
    );

    // Anonymous user: on update, immutable_field should be denied (admin_only denies anonymous)
    let denied_on_update = runner.check_field_write_access(&fields, None, "update", &conn);
    assert!(
        denied_on_update.contains(&"immutable_field".to_string()),
        "immutable_field should be denied on update for anonymous, got: {:?}",
        denied_on_update
    );
    assert!(
        !denied_on_update.contains(&"auto_slug".to_string()),
        "auto_slug should be allowed on update (no update access config), got: {:?}",
        denied_on_update
    );
}

#[test]
fn no_access_config_means_allowed() {
    // Verify that when no access config is set (None), the result is Allowed.
    // This tests backward compatibility: existing setups without access control
    // should remain fully open.
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();

    // Collection-level: None access ref should return Allowed
    let result = runner.check_access(None, None, None, None, &conn).unwrap();
    assert!(
        matches!(result, query::AccessResult::Allowed),
        "None access ref should return Allowed, got: {:?}",
        result
    );

    // Also test with a user present — should still be Allowed
    let editor = make_user_doc("editor-1", "editor");
    let result = runner
        .check_access(None, Some(&editor), None, None, &conn)
        .unwrap();
    assert!(
        matches!(result, query::AccessResult::Allowed),
        "None access ref with user should still return Allowed, got: {:?}",
        result
    );

    // Field-level: fields without any access config should not be denied
    // access.read, access.create, access.update all None by default
    let fields = vec![crap_cms::core::FieldDefinition {
        name: "open_field".to_string(),
        field_type: crap_cms::core::field::FieldType::Text,
        ..Default::default()
    }];

    let denied_read = runner.check_field_read_access(&fields, None, &conn);
    assert!(
        denied_read.is_empty(),
        "Fields with no access config should not be denied for read"
    );

    let denied_write_create = runner.check_field_write_access(&fields, None, "create", &conn);
    assert!(
        denied_write_create.is_empty(),
        "Fields with no access config should not be denied for create"
    );

    let denied_write_update = runner.check_field_write_access(&fields, None, "update", &conn);
    assert!(
        denied_write_update.is_empty(),
        "Fields with no access config should not be denied for update"
    );
}

// ── 6. Row-level enforcement of `Constrained` on write paths ───────────────
//
// Fix B: when a write access hook returns `Constrained(filters)`, the service
// layer must look up the target row and reject the write when the filters
// don't match. Previously only `Denied` was checked and `Constrained` was
// silently treated as "allow" on writes.
//
// These tests use a dedicated fixture at `tests/fixtures/access_row_enforcement`
// whose `articles` collection wires `own_rows` (returns `{ author_id = id }`)
// to update/delete/trash, and a separate `bad_create_articles` collection
// whose `create` access mistakenly returns a filter table.

fn row_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/access_row_enforcement")
}

fn row_setup() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    crap_cms::core::SharedRegistry,
    HookRunner,
) {
    let config_dir = row_fixture_dir();
    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(&config_dir, &config).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry.clone())
        .config(&config)
        .build()
        .unwrap();
    (tmp, db_pool, registry, runner)
}

/// Seed articles directly via `query::create` to bypass access checks.
fn seed_article(
    pool: &crap_cms::db::DbPool,
    registry: &crap_cms::core::SharedRegistry,
    slug: &str,
    author_id: &str,
    title: &str,
) -> String {
    let reg = registry.read().unwrap();
    let def = reg.get_collection(slug).unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), title.to_string());
    data.insert("author_id".to_string(), author_id.to_string());

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let doc = query::create(&tx, slug, &def, &data, None).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

#[test]
fn access_hook_filter_table_on_update_denies_when_filter_does_not_match() {
    let (_tmp, pool, registry, runner) = row_setup();
    // Article belongs to user_b
    let id = seed_article(&pool, &registry, "articles", "user_b", "Original");
    let user_a = make_user_doc("user_a", "editor");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        local doc = crap.collections.update("articles", "{}", {{ title = "Hacked" }})
        return doc.title
        "#,
        id
    );
    let result = runner.eval_lua_with_conn(&code, &conn, Some(&user_a));

    assert!(
        result.is_err(),
        "user_a should not be able to update user_b's row via Constrained hook"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("access denied") || err.contains("Update access denied"),
        "error should mention access denied, got: {err}"
    );
}

#[test]
fn access_hook_filter_table_on_update_allows_when_filter_matches() {
    let (_tmp, pool, registry, runner) = row_setup();
    // Article belongs to user_a — filter { author_id = user_a } matches
    let id = seed_article(&pool, &registry, "articles", "user_a", "Original");
    let user_a = make_user_doc("user_a", "editor");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        local doc = crap.collections.update("articles", "{}", {{ title = "Mine Updated" }})
        return doc.title
        "#,
        id
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&user_a))
        .expect("update should succeed when filter matches");
    assert_eq!(result, "Mine Updated");
}

#[test]
fn access_hook_filter_table_on_delete_denies_when_filter_does_not_match() {
    let (_tmp, pool, registry, runner) = row_setup();
    // `articles` has soft_delete = true → delete runs the trash hook (own_rows).
    let id = seed_article(&pool, &registry, "articles", "user_b", "Other's article");
    let user_a = make_user_doc("user_a", "editor");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        crap.collections.delete("articles", "{}")
        return "OK"
        "#,
        id
    );
    let result = runner.eval_lua_with_conn(&code, &conn, Some(&user_a));

    assert!(
        result.is_err(),
        "user_a should not be able to delete user_b's row via Constrained hook"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("access denied")
            || err.contains("Trash access denied")
            || err.contains("Delete access denied"),
        "error should mention access denied, got: {err}"
    );
}

#[test]
fn access_hook_filter_table_on_create_is_rejected_with_clear_error() {
    let (_tmp, pool, _registry, runner) = row_setup();
    let user_a = make_user_doc("user_a", "editor");

    let conn = pool.get().unwrap();
    let result = runner.eval_lua_with_conn(
        r#"
        local doc = crap.collections.create("bad_create_articles", {
            title = "Hello",
            author_id = "user_a",
        })
        return doc.id
        "#,
        &conn,
        Some(&user_a),
    );

    assert!(
        result.is_err(),
        "Constrained on create must be rejected with a clear error"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("create") && err.contains("filter table"),
        "error should name the operation and mention 'filter table', got: {err}"
    );
    assert!(
        err.contains("bad_create_articles"),
        "error should name the slug, got: {err}"
    );
    assert!(
        err.contains("true/false") || err.contains("return true"),
        "error should hint at the correct return value, got: {err}"
    );
}

#[test]
fn access_hook_filter_table_on_undelete_enforces_match() {
    let (_tmp, pool, registry, runner) = row_setup();
    let id = seed_article(&pool, &registry, "articles", "user_b", "Trashed");

    // First soft-delete the row (using override so the delete is not blocked).
    {
        let conn = pool.get().unwrap();
        let code = format!(
            r#"
            crap.collections.delete("articles", "{}", {{ overrideAccess = true }})
            return "OK"
            "#,
            id
        );
        runner.eval_lua_with_conn(&code, &conn, None).unwrap();
    }

    // Now user_a tries to undelete user_b's row — Constrained filter should reject.
    let user_a = make_user_doc("user_a", "editor");
    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        crap.collections.undelete("articles", "{}")
        return "OK"
        "#,
        id
    );
    let result = runner.eval_lua_with_conn(&code, &conn, Some(&user_a));

    assert!(
        result.is_err(),
        "user_a should not be able to undelete user_b's trashed row"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("access denied") || err.contains("Undelete access denied"),
        "error should mention access denied, got: {err}"
    );

    // But user_b (the real author) should succeed.
    let user_b = make_user_doc("user_b", "editor");
    let code = format!(
        r#"
        crap.collections.undelete("articles", "{}")
        return "OK"
        "#,
        id
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&user_b))
        .expect("user_b (matching author_id) should be allowed to undelete");
    assert_eq!(result, "OK");
}

/// Regression: HookRunner::check_access must consult DefaultDeny when access_ref is None.
/// Previously, check_access short-circuited to Allowed before reaching the DefaultDeny check.
#[test]
fn default_deny_true_no_access_ref_returns_denied() {
    let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("example");
    let mut config = CrapConfig::default();
    config.access.default_deny = true;

    let registry = hooks::init_lua(&config_dir, &config).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry.clone())
        .config(&config)
        .build()
        .unwrap();

    let conn = db_pool.get().unwrap();

    // No access function configured + default_deny = true → must be Denied
    let result = runner.check_access(None, None, None, None, &conn).unwrap();
    assert!(
        matches!(result, query::AccessResult::Denied),
        "With default_deny=true and no access ref, expected Denied, got: {:?}",
        result
    );

    // Even with a user present, no access function + default_deny → Denied
    let user = make_user_doc("user-1", "editor");
    let result = runner
        .check_access(None, Some(&user), None, None, &conn)
        .unwrap();
    assert!(
        matches!(result, query::AccessResult::Denied),
        "With default_deny=true, user present, and no access ref, expected Denied, got: {:?}",
        result
    );
}

// ── 7. Constrained from access hooks rejected/enforced on the remaining paths ──
//
// The fixture at `tests/fixtures/access_row_enforcement` includes:
// - the `site_settings` global wired to `own_rows` (returns a filter table)
// - a `constrained_job` job wired to `own_rows`
// - a `versioned_articles` collection whose read/update use `own_rows`
// These tests close the remaining gaps where Constrained was silently dropped
// (globals, version list/restore, job trigger).

#[test]
fn access_hook_filter_table_on_global_read_is_rejected() {
    let (_tmp, pool, registry, runner) = row_setup();
    let user_a = make_user_doc("user_a", "editor");

    let reg = registry.read().unwrap();
    let def = reg.get_global("site_settings").unwrap().clone();
    drop(reg);

    let conn = pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&runner, &conn);
    let ctx = ServiceContext::global("site_settings", &def)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(Some(&user_a))
        .build();
    let input = GetGlobalInput::new(None, None);

    let err =
        get_global_document(&ctx, &input).expect_err("Constrained on global read must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("site_settings"), "got: {msg}");
    assert!(msg.contains("filter table"), "got: {msg}");
    assert!(msg.contains("global"), "got: {msg}");
}

#[test]
fn access_hook_filter_table_on_global_update_is_rejected() {
    let (_tmp, pool, registry, runner) = row_setup();
    let user_a = make_user_doc("user_a", "editor");

    let reg = registry.read().unwrap();
    let def = reg.get_global("site_settings").unwrap().clone();
    drop(reg);

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
    let ctx = ServiceContext::global("site_settings", &def)
        .conn(&tx)
        .write_hooks(&wh)
        .user(Some(&user_a))
        .build();

    let mut data = HashMap::new();
    data.insert("site_name".to_string(), "Hacked".to_string());
    let join_data = HashMap::new();
    let input = WriteInput::builder(data, &join_data).build();

    let err =
        update_global_core(&ctx, input).expect_err("Constrained on global update must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("site_settings"), "got: {msg}");
    assert!(msg.contains("filter table"), "got: {msg}");
}

/// Helper: seed a versioned article directly so we can drive list/restore.
/// Returns (doc_id, first_version_id).
fn seed_versioned_article(
    pool: &crap_cms::db::DbPool,
    registry: &crap_cms::core::SharedRegistry,
    author_id: &str,
    title: &str,
) -> (String, String) {
    let reg = registry.read().unwrap();
    let def = reg.get_collection("versioned_articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), title.to_string());
    data.insert("author_id".to_string(), author_id.to_string());

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let doc = query::create(&tx, "versioned_articles", &def, &data, None).unwrap();

    // Create a version snapshot directly via the query layer.
    let snapshot_json = json!({
        "title": title,
        "author_id": author_id,
    });
    let version = query::create_version(
        &tx,
        "versioned_articles",
        &doc.id,
        "published",
        &snapshot_json,
    )
    .unwrap();

    tx.commit().unwrap();
    (doc.id.to_string(), version.id)
}

#[test]
fn access_hook_filter_table_on_list_versions_enforces_parent_match() {
    let (_tmp, pool, registry, runner) = row_setup();
    let (id, _) = seed_versioned_article(&pool, &registry, "user_b", "Other's doc");

    let reg = registry.read().unwrap();
    let def = reg.get_collection("versioned_articles").unwrap().clone();
    drop(reg);

    // user_a (not the author) should be denied because own_rows enforces
    // { author_id = user_a } against the parent row, which doesn't match.
    let user_a = make_user_doc("user_a", "editor");
    let conn = pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&runner, &conn);
    let ctx = ServiceContext::collection("versioned_articles", &def)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(Some(&user_a))
        .build();
    let input = ListVersionsInput::builder(&id).build();
    let err = list_versions(&ctx, &input)
        .err()
        .expect("user_a must not list versions of user_b's row");
    let msg = err.to_string();
    assert!(
        msg.contains("access denied") || msg.contains("Read access denied"),
        "got: {msg}"
    );

    // user_b (matching author) should succeed.
    let user_b = make_user_doc("user_b", "editor");
    let hooks_b = RunnerReadHooks::new(&runner, &conn);
    let ctx_b = ServiceContext::collection("versioned_articles", &def)
        .conn(&conn)
        .read_hooks(&hooks_b)
        .user(Some(&user_b))
        .build();
    let input_b = ListVersionsInput::builder(&id).build();
    let result = list_versions(&ctx_b, &input_b).expect("user_b should list versions");
    assert!(
        !result.docs.is_empty(),
        "user_b should see at least one version"
    );
}

#[test]
fn access_hook_filter_table_on_restore_version_enforces_parent_match() {
    let (_tmp, pool, registry, runner) = row_setup();
    let (id, version_id) = seed_versioned_article(&pool, &registry, "user_b", "Other's doc");

    let reg = registry.read().unwrap();
    let def = reg.get_collection("versioned_articles").unwrap().clone();
    drop(reg);
    let lc = LocaleConfig::default();

    // user_a must not restore user_b's version — Constrained { author_id = user_a }
    // doesn't match the parent row (author_id = user_b).
    let user_a = make_user_doc("user_a", "editor");
    let ctx_a = ServiceContext::collection("versioned_articles", &def)
        .pool(&pool)
        .user(Some(&user_a))
        .runner(&runner)
        .build();
    let err = restore_collection_version(&ctx_a, &id, &version_id, &lc)
        .expect_err("user_a must not restore user_b's version");
    let msg = err.to_string();
    assert!(
        msg.contains("access denied") || msg.contains("Update access denied"),
        "got: {msg}"
    );

    // user_b (matching author) should succeed.
    let user_b = make_user_doc("user_b", "editor");
    let ctx_b = ServiceContext::collection("versioned_articles", &def)
        .pool(&pool)
        .user(Some(&user_b))
        .runner(&runner)
        .build();
    restore_collection_version(&ctx_b, &id, &version_id, &lc)
        .expect("user_b should restore their own version");
}

/// Regression: a version snapshot whose data violates current schema
/// constraints (e.g. a `required` field is empty) must be rejected at
/// restore time. Previously `query::restore_version` wrote the snapshot
/// directly via raw `update` without running schema validation, so an old
/// snapshot from before a `required = true` tightening would restore into
/// an invalid state. The restore path now runs `validate_fields` before
/// the write, surfacing the violation as a `ValidationError`.
#[test]
fn restore_collection_version_rejects_snapshot_violating_required_field() {
    let (_tmp, pool, registry, runner) = row_setup();

    let reg = registry.read().unwrap();
    let def = reg.get_collection("versioned_articles").unwrap().clone();
    drop(reg);

    // Seed a valid live row so restore has a target.
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Original".to_string());
    data.insert("author_id".to_string(), "user_b".to_string());
    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let doc = query::create(&tx, "versioned_articles", &def, &data, None).unwrap();

    // Hand-craft a version snapshot with empty title — violates `required`.
    let bad_snapshot = json!({
        "title": "",
        "author_id": "user_b",
    });
    let version =
        query::create_version(&tx, "versioned_articles", &doc.id, "draft", &bad_snapshot).unwrap();
    tx.commit().unwrap();

    let user_b = make_user_doc("user_b", "editor");
    let lc = LocaleConfig::default();
    let ctx = ServiceContext::collection("versioned_articles", &def)
        .pool(&pool)
        .user(Some(&user_b))
        .runner(&runner)
        .build();

    let err = restore_collection_version(&ctx, &doc.id, &version.id, &lc)
        .expect_err("restore must reject snapshot with empty required field");
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("title")
            || msg.to_lowercase().contains("required")
            || msg.to_lowercase().contains("validation"),
        "expected validation error mentioning title/required, got: {msg}"
    );

    // Live row must still hold the original (untouched) value.
    let live = query::find_by_id(
        &pool.get().unwrap(),
        "versioned_articles",
        &def,
        &doc.id,
        None,
    )
    .unwrap()
    .expect("live row still present");
    assert_eq!(
        live.fields.get("title").and_then(|v| v.as_str()),
        Some("Original")
    );
}

#[test]
fn access_hook_filter_table_on_job_trigger_is_rejected() {
    let (_tmp, pool, registry, runner) = row_setup();
    let user_a = make_user_doc("user_a", "editor");

    let reg = registry.read().unwrap();
    let job_def = reg.get_job("constrained_job").unwrap().clone();
    drop(reg);

    let conn = pool.get().unwrap();
    let ctx = ServiceContext::slug_only("constrained_job")
        .conn(&conn)
        .runner(&runner)
        .user(Some(&user_a))
        .build();

    let input = QueueJobInput {
        job_def: &job_def,
        data: None,
        scheduled_by: "test",
    };

    let err = queue_job(&ctx, &input).expect_err("Constrained on job trigger must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("constrained_job"), "got: {msg}");
    assert!(msg.contains("filter table"), "got: {msg}");
    assert!(
        msg.contains("trigger-only") || msg.contains("ctx.user"),
        "got: {msg}"
    );
}

/// Helper: seed a versioned article directly at a specific `_status` value.
/// `_status = "published"` → visible with default search; `"draft"` → hidden
/// unless `include_drafts = true`.
fn seed_versioned_article_with_status(
    pool: &crap_cms::db::DbPool,
    registry: &crap_cms::core::SharedRegistry,
    author_id: &str,
    title: &str,
    status: &str,
) -> String {
    let reg = registry.read().unwrap();
    let def = reg.get_collection("versioned_articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), title.to_string());
    data.insert("author_id".to_string(), author_id.to_string());

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let doc = query::create(&tx, "versioned_articles", &def, &data, None).unwrap();
    tx.execute(
        r#"UPDATE "versioned_articles" SET _status = ?1 WHERE id = ?2"#,
        &[
            crap_cms::db::DbValue::Text(status.to_string()),
            crap_cms::db::DbValue::Text(doc.id.to_string()),
        ],
    )
    .unwrap();
    tx.commit().unwrap();

    doc.id.to_string()
}

#[test]
fn search_documents_excludes_drafts_by_default() {
    // A drafts-enabled collection with one published and one draft row.
    // With `include_drafts = false`, only the published row is returned.
    // user_a is the author of both so `own_rows` returns `Allowed` — this
    // test isolates the `_status = "published"` injection behaviour.
    let (_tmp, pool, registry, runner) = row_setup();
    let _published =
        seed_versioned_article_with_status(&pool, &registry, "user_a", "Published", "published");
    let _draft = seed_versioned_article_with_status(&pool, &registry, "user_a", "Draft", "draft");

    let reg = registry.read().unwrap();
    let def = reg.get_collection("versioned_articles").unwrap().clone();
    drop(reg);

    let user_a = make_user_doc("user_a", "editor");
    let conn = pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&runner, &conn);
    let ctx = ServiceContext::collection("versioned_articles", &def)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(Some(&user_a))
        .build();

    let fq = FindQuery::default();
    let input = SearchDocumentsInput {
        query: &fq,
        locale_ctx: None,
        cursor_enabled: false,
        include_drafts: false,
    };

    let result = search_documents(&ctx, &input).expect("search ok");
    assert_eq!(
        result.docs.len(),
        1,
        "should only see the published row; got {}",
        result.docs.len()
    );
    assert_eq!(
        result.docs[0].get_str("title"),
        Some("Published"),
        "expected the published row"
    );
}

#[test]
fn search_documents_includes_drafts_when_opted_in() {
    // Same setup as above but `include_drafts = true` — admin-picker shape.
    // Both rows are returned regardless of `_status`.
    let (_tmp, pool, registry, runner) = row_setup();
    let _published =
        seed_versioned_article_with_status(&pool, &registry, "user_a", "Published", "published");
    let _draft = seed_versioned_article_with_status(&pool, &registry, "user_a", "Draft", "draft");

    let reg = registry.read().unwrap();
    let def = reg.get_collection("versioned_articles").unwrap().clone();
    drop(reg);

    let user_a = make_user_doc("user_a", "editor");
    let conn = pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&runner, &conn);
    let ctx = ServiceContext::collection("versioned_articles", &def)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(Some(&user_a))
        .build();

    let fq = FindQuery::default();
    let input = SearchDocumentsInput {
        query: &fq,
        locale_ctx: None,
        cursor_enabled: false,
        include_drafts: true,
    };

    let result = search_documents(&ctx, &input).expect("search ok");
    assert_eq!(
        result.docs.len(),
        2,
        "admin picker (include_drafts=true) should see draft + published; got {}",
        result.docs.len()
    );
}

// ── 8. Lua `crap.collections.delete` respects `forceHardDelete` on soft-delete collections ──
//
// The `articles` collection in the row-enforcement fixture has `soft_delete = true`.
// `forceHardDelete = true` must actually hard-delete the row (previously was
// silently soft-deleted). The default (no opt) must still soft-delete: row stays
// present with `_deleted_at` set.

/// Helper: query the raw row for an article (bypasses the soft-delete filter).
/// Returns (row_exists, deleted_at_is_set).
fn raw_article_row_state(pool: &crap_cms::db::DbPool, id: &str) -> (bool, bool) {
    let conn = pool.get().unwrap();
    let sql = format!(
        "SELECT _deleted_at FROM articles WHERE id = {}",
        conn.placeholder(1)
    );
    let row = conn
        .query_one(&sql, &[DbValue::Text(id.to_string())])
        .unwrap();
    match row {
        Some(r) => {
            let has_deleted = r.get_opt_string("_deleted_at").unwrap_or(None).is_some();
            (true, has_deleted)
        }
        None => (false, false),
    }
}

#[test]
fn lua_delete_force_hard_delete_removes_row_on_soft_delete_collection() {
    let (_tmp, pool, registry, runner) = row_setup();
    // Seed as user_a (author) so the own_rows access hook allows the delete.
    let id = seed_article(&pool, &registry, "articles", "user_a", "To hard-delete");
    let user_a = make_user_doc("user_a", "editor");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        crap.collections.delete("articles", "{}", {{ forceHardDelete = true }})
        return "OK"
        "#,
        id
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&user_a))
        .expect("forceHardDelete on soft-delete collection should succeed");
    assert_eq!(result, "OK");

    // Row must be GONE from the DB (not just _deleted_at set).
    drop(conn);
    let (exists, _) = raw_article_row_state(&pool, &id);
    assert!(
        !exists,
        "forceHardDelete = true must hard-delete the row; row still present"
    );
}

#[test]
fn lua_delete_default_soft_deletes_row_on_soft_delete_collection() {
    let (_tmp, pool, registry, runner) = row_setup();
    // Seed as user_a (author) so the own_rows access hook allows the delete.
    let id = seed_article(&pool, &registry, "articles", "user_a", "To soft-delete");
    let user_a = make_user_doc("user_a", "editor");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        crap.collections.delete("articles", "{}")
        return "OK"
        "#,
        id
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&user_a))
        .expect("default delete on soft-delete collection should succeed");
    assert_eq!(result, "OK");

    // Row must still exist, with _deleted_at populated.
    drop(conn);
    let (exists, has_deleted_at) = raw_article_row_state(&pool, &id);
    assert!(
        exists,
        "default delete must keep the row (soft-delete); row missing"
    );
    assert!(has_deleted_at, "soft-deleted row must have _deleted_at set");
}

#[test]
fn lua_delete_force_hard_delete_false_soft_deletes() {
    // Explicit `forceHardDelete = false` must behave like the default — soft-delete.
    let (_tmp, pool, registry, runner) = row_setup();
    let id = seed_article(&pool, &registry, "articles", "user_a", "Explicit false");
    let user_a = make_user_doc("user_a", "editor");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        crap.collections.delete("articles", "{}", {{ forceHardDelete = false }})
        return "OK"
        "#,
        id
    );
    runner
        .eval_lua_with_conn(&code, &conn, Some(&user_a))
        .expect("delete with forceHardDelete = false should succeed");
    drop(conn);

    let (exists, has_deleted_at) = raw_article_row_state(&pool, &id);
    assert!(
        exists,
        "forceHardDelete = false must preserve the row (soft-delete); row missing"
    );
    assert!(has_deleted_at, "soft-deleted row must have _deleted_at set");
}

// ── 9. Lua `crap.collections.list_versions` / `restore_version` respect access by default ──
//
// Both list_versions and restore_version used to hardcode `override_access = true`.
// They now go through the normal access path unless `opts.overrideAccess = true`.
// Regression: make sure a non-owner is denied by default and can bypass via opts.

#[test]
fn lua_list_versions_respects_access_by_default() {
    let (_tmp, pool, registry, runner) = row_setup();
    // Authored by user_b → versioned_articles has `read = own_rows`.
    let (id, _version_id) = seed_versioned_article(&pool, &registry, "user_b", "B's doc");
    let user_a = make_user_doc("user_a", "editor");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        local r = crap.collections.list_versions("versioned_articles", "{}")
        return "OK:" .. tostring(#r.docs)
        "#,
        id
    );
    let result = runner.eval_lua_with_conn(&code, &conn, Some(&user_a));
    assert!(
        result.is_err(),
        "user_a should be denied list_versions on user_b's row"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("access denied") || err.contains("Read access denied"),
        "error should mention access denied, got: {err}"
    );
}

#[test]
fn lua_list_versions_override_access_bypasses() {
    let (_tmp, pool, registry, runner) = row_setup();
    let (id, _version_id) = seed_versioned_article(&pool, &registry, "user_b", "B's doc");
    let user_a = make_user_doc("user_a", "editor");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        local r = crap.collections.list_versions(
            "versioned_articles", "{}", {{ overrideAccess = true }}
        )
        return tostring(#r.docs)
        "#,
        id
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&user_a))
        .expect("overrideAccess = true must bypass the access check");
    let count: usize = result.parse().expect("docs count should be numeric");
    assert!(
        count >= 1,
        "expected at least one version with overrideAccess = true, got: {result}"
    );
}

#[test]
fn lua_restore_version_respects_access_by_default() {
    let (_tmp, pool, registry, runner) = row_setup();
    let (id, version_id) = seed_versioned_article(&pool, &registry, "user_b", "B's doc");
    let user_a = make_user_doc("user_a", "editor");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        crap.collections.restore_version("versioned_articles", "{}", "{}")
        return "OK"
        "#,
        id, version_id
    );
    let result = runner.eval_lua_with_conn(&code, &conn, Some(&user_a));
    assert!(
        result.is_err(),
        "user_a should be denied restore_version on user_b's row"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("access denied") || err.contains("Update access denied"),
        "error should mention access denied, got: {err}"
    );
}

#[test]
fn lua_restore_version_override_access_bypasses() {
    let (_tmp, pool, registry, runner) = row_setup();
    let (id, version_id) = seed_versioned_article(&pool, &registry, "user_b", "B's doc");
    let user_a = make_user_doc("user_a", "editor");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        local d = crap.collections.restore_version(
            "versioned_articles", "{}", "{}",
            {{ overrideAccess = true }}
        )
        return d.id
        "#,
        id, version_id
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&user_a))
        .expect("overrideAccess = true must bypass the access check");
    assert_eq!(result, id, "restored doc id must match parent doc id");
}
