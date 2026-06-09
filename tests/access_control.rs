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

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::Document;
use crap_cms::core::DocumentFields;
use crap_cms::core::HookRef;
use crap_cms::db::{DbConnection, FindQuery};
use crap_cms::db::{DbValue, migrate, ops, pool, query};
use crap_cms::hooks;
use crap_cms::hooks::AccessCheckInput;
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{
    GetGlobalInput, ListVersionsInput, RunnerReadHooks, RunnerWriteHooks, SearchDocumentsInput,
    ServiceContext, WriteInput, create_document_in_conn, get_global_document,
    jobs::{QueueJobInput, queue_job},
    list_versions, restore_collection_version, search_documents, update_document,
    update_global_in_conn,
};
use serde_json::{Value, json};

fn setup() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    std::sync::Arc<crap_cms::core::Registry>,
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
        .registry(Arc::clone(&registry))
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
    let posts = registry
        .get_collection("posts")
        .expect("posts collection not found");

    assert_eq!(
        posts.access.read.as_ref().map(HookRef::reference),
        Some("access.published_or_author")
    );
    assert_eq!(
        posts.access.create.as_ref().map(HookRef::reference),
        Some("access.authenticated")
    );
    assert_eq!(
        posts.access.update.as_ref().map(HookRef::reference),
        Some("access.author_or_editor")
    );
    assert_eq!(
        posts.access.trash.as_ref().map(HookRef::reference),
        Some("access.editor_or_above")
    );
    assert_eq!(
        posts.access.delete.as_ref().map(HookRef::reference),
        Some("access.admin_or_director")
    );
}

#[test]
fn field_access_parsed_from_lua() {
    let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("example");
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).unwrap();
    let posts = registry
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

    let result = runner
        .check_access(
            &AccessCheckInput {
                access: None,
                user: None,
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
            &conn,
        )
        .unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));
}

#[test]
fn anyone_allows_anonymous() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();

    let result = runner
        .check_access(
            &AccessCheckInput {
                access: Some(&HookRef::new("access.anyone")),
                user: None,
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
            &conn,
        )
        .unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));
}

#[test]
fn authenticated_denies_anonymous() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();

    let result = runner
        .check_access(
            &AccessCheckInput {
                access: Some(&HookRef::new("access.authenticated")),
                user: None,
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
            &conn,
        )
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
            &AccessCheckInput {
                access: Some(&HookRef::new("access.authenticated")),
                user: Some(&editor),
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
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
        .check_access(
            &AccessCheckInput {
                access: Some(&HookRef::new("access.admin_only")),
                user: Some(&editor),
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
            &conn,
        )
        .unwrap();
    assert!(matches!(result, query::AccessResult::Denied));
}

#[test]
fn admin_only_allows_admin() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();
    let admin = make_user_doc("admin-1", "admin");

    let result = runner
        .check_access(
            &AccessCheckInput {
                access: Some(&HookRef::new("access.admin_only")),
                user: Some(&admin),
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
            &conn,
        )
        .unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));
}

#[test]
fn published_or_author_constrains_anonymous() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();

    let result = runner
        .check_access(
            &AccessCheckInput {
                access: Some(&HookRef::new("access.published_or_author")),
                user: None,
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
            &conn,
        )
        .unwrap();

    match result {
        query::AccessResult::Constrained(clauses) => {
            assert_eq!(clauses.len(), 1);
            match &clauses[0] {
                query::FilterClause::Single(f) => {
                    assert_eq!(f.field, "_status");
                    match &f.op {
                        query::FilterOp::Equals(val) => assert_eq!(val, "published"),
                        other => panic!("Expected Equals op, got {other:?}"),
                    }
                }
                other => panic!("Expected Single clause, got {other:?}"),
            }
        }
        other => panic!("Expected Constrained, got {other:?}"),
    }
}

#[test]
fn published_or_author_allows_admin() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();
    let admin = make_user_doc("admin-1", "admin");

    let result = runner
        .check_access(
            &AccessCheckInput {
                access: Some(&HookRef::new("access.published_or_author")),
                user: Some(&admin),
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
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

    let posts = registry.get_collection("posts").unwrap();

    let denied =
        runner.check_field_write_access(&posts.fields, Some(&editor), None, "update", &conn);
    // No field-level access controls in posts definition
    assert!(
        denied.is_empty(),
        "Expected no denied fields, got: {denied:?}"
    );
}

#[test]
fn field_read_no_config_allows_all() {
    let (_tmp, pool, registry, runner) = setup();
    let conn = pool.get().unwrap();

    let posts = registry.get_collection("posts").unwrap();

    // No field has read access configured, so nothing should be denied
    let denied = runner.check_field_read_access(&posts.fields, None, None, &conn);
    assert!(
        denied.is_empty(),
        "Expected no denied fields for read, got: {denied:?}"
    );
}

// ── 4. End-to-End with DB ───────────────────────────────────────────────────

#[test]
fn constrained_find_filters_results() {
    let (_tmp, pool, registry, _runner) = setup();

    let posts = registry.get_collection("posts").unwrap().clone();

    // Create posts with different _status values via the versioning system
    let post_data = vec![
        ("Alpha Post", "alpha-post", "draft"),
        ("Beta Post", "beta-post", "published"),
        ("Gamma Post", "gamma-post", "draft"),
        ("Delta Post", "delta-post", "published"),
    ];
    for (title, slug, status) in &post_data {
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!(title));
        data.insert("slug".to_string(), json!(slug));
        data.insert("excerpt".to_string(), json!("Test excerpt"));

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

    let posts = registry.get_collection("posts").unwrap().clone();

    // Create some posts
    for (i, slug) in ["e2e-post-1", "e2e-post-2"].iter().enumerate() {
        let mut data = DocumentFields::new();
        data.insert(
            "title".to_string(),
            Value::String(format!("E2E Post {}", i + 1)),
        );
        data.insert("slug".to_string(), json!(slug));
        data.insert("excerpt".to_string(), json!("Test excerpt"));

        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "posts", &posts, &data, None).unwrap();
        query::set_document_status(&tx, "posts", &doc.id, "published").unwrap();
        tx.commit().unwrap();
    }

    // Verify published_or_author for anonymous returns constraint (not fully open)
    let conn = pool.get().unwrap();
    let result = runner
        .check_access(
            &AccessCheckInput {
                access: posts.access.read.as_ref(),
                user: None,
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
            &conn,
        )
        .unwrap();
    assert!(matches!(result, query::AccessResult::Constrained(_)));

    // Verify admin gets full access
    let admin = make_user_doc("admin-1", "admin");
    let result = runner
        .check_access(
            &AccessCheckInput {
                access: posts.access.read.as_ref(),
                user: Some(&admin),
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
            &conn,
        )
        .unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));

    let all_docs =
        ops::find_documents(&pool, "posts", &posts, &query::FindQuery::default(), None).unwrap();
    assert_eq!(all_docs.len(), 2);

    // Verify author_or_admin denies anonymous delete
    let result = runner
        .check_access(
            &AccessCheckInput {
                access: posts.access.delete.as_ref(),
                user: None,
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
            &conn,
        )
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
                read: Some(HookRef::new("access.admin_only")),
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
    let denied = runner.check_field_read_access(&fields, None, None, &conn);
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
    let denied = runner.check_field_read_access(&fields, Some(&admin), None, &conn);
    assert!(
        denied.is_empty(),
        "Admin user should have all fields allowed, but got denied: {denied:?}"
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
                create: Some(HookRef::new("access.admin_only")),
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
                update: Some(HookRef::new("access.admin_only")),
                ..Default::default()
            },
            ..Default::default()
        },
    ];

    // Anonymous user: on create, auto_slug should be denied (admin_only denies anonymous)
    let denied_on_create = runner.check_field_write_access(&fields, None, None, "create", &conn);
    assert!(
        denied_on_create.contains(&"auto_slug".to_string()),
        "auto_slug should be denied on create for anonymous, got: {denied_on_create:?}"
    );
    assert!(
        !denied_on_create.contains(&"immutable_field".to_string()),
        "immutable_field should be allowed on create (no create access config), got: {denied_on_create:?}"
    );

    // Anonymous user: on update, immutable_field should be denied (admin_only denies anonymous)
    let denied_on_update = runner.check_field_write_access(&fields, None, None, "update", &conn);
    assert!(
        denied_on_update.contains(&"immutable_field".to_string()),
        "immutable_field should be denied on update for anonymous, got: {denied_on_update:?}"
    );
    assert!(
        !denied_on_update.contains(&"auto_slug".to_string()),
        "auto_slug should be allowed on update (no update access config), got: {denied_on_update:?}"
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
    let result = runner
        .check_access(
            &AccessCheckInput {
                access: None,
                user: None,
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
            &conn,
        )
        .unwrap();
    assert!(
        matches!(result, query::AccessResult::Allowed),
        "None access ref should return Allowed, got: {result:?}"
    );

    // Also test with a user present — should still be Allowed
    let editor = make_user_doc("editor-1", "editor");
    let result = runner
        .check_access(
            &AccessCheckInput {
                access: None,
                user: Some(&editor),
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
            &conn,
        )
        .unwrap();
    assert!(
        matches!(result, query::AccessResult::Allowed),
        "None access ref with user should still return Allowed, got: {result:?}"
    );

    // Field-level: fields without any access config should not be denied
    // access.read, access.create, access.update all None by default
    let fields = vec![crap_cms::core::FieldDefinition {
        name: "open_field".to_string(),
        field_type: crap_cms::core::field::FieldType::Text,
        ..Default::default()
    }];

    let denied_read = runner.check_field_read_access(&fields, None, None, &conn);
    assert!(
        denied_read.is_empty(),
        "Fields with no access config should not be denied for read"
    );

    let denied_write_create = runner.check_field_write_access(&fields, None, None, "create", &conn);
    assert!(
        denied_write_create.is_empty(),
        "Fields with no access config should not be denied for create"
    );

    let denied_write_update = runner.check_field_write_access(&fields, None, None, "update", &conn);
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
    std::sync::Arc<crap_cms::core::Registry>,
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
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();
    (tmp, db_pool, registry, runner)
}

/// Seed articles directly via `query::create` to bypass access checks.
fn seed_article(
    pool: &crap_cms::db::DbPool,
    registry: &std::sync::Arc<crap_cms::core::Registry>,
    slug: &str,
    author_id: &str,
    title: &str,
) -> String {
    let def = registry.get_collection(slug).unwrap().clone();

    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!(title));
    data.insert("author_id".to_string(), json!(author_id));

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
        local doc = crap.collections.update("articles", "{id}", {{ title = "Hacked" }})
        return doc.title
        "#
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
        local doc = crap.collections.update("articles", "{id}", {{ title = "Mine Updated" }})
        return doc.title
        "#
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
        crap.collections.delete("articles", "{id}")
        return "OK"
        "#
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
            crap.collections.delete("articles", "{id}", {{ overrideAccess = true }})
            return "OK"
            "#
        );
        runner.eval_lua_with_conn(&code, &conn, None).unwrap();
    }

    // Now user_a tries to undelete user_b's row — Constrained filter should reject.
    let user_a = make_user_doc("user_a", "editor");
    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        crap.collections.undelete("articles", "{id}")
        return "OK"
        "#
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
        crap.collections.undelete("articles", "{id}")
        return "OK"
        "#
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&user_b))
        .expect("user_b (matching author_id) should be allowed to undelete");
    assert_eq!(result, "OK");
}

/// Regression: `HookRunner::check_access` must consult `DefaultDeny` when `access_ref` is None.
/// Previously, `check_access` short-circuited to Allowed before reaching the `DefaultDeny` check.
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
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let conn = db_pool.get().unwrap();

    // No access function configured + default_deny = true → must be Denied
    let result = runner
        .check_access(
            &AccessCheckInput {
                access: None,
                user: None,
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
            &conn,
        )
        .unwrap();
    assert!(
        matches!(result, query::AccessResult::Denied),
        "With default_deny=true and no access ref, expected Denied, got: {result:?}"
    );

    // Even with a user present, no access function + default_deny → Denied
    let user = make_user_doc("user-1", "editor");
    let result = runner
        .check_access(
            &AccessCheckInput {
                access: None,
                user: Some(&user),
                id: None,
                data: None,
                locale: None,
                operation: "find",
                collection: "test",
                ui_locale: None,
            },
            &conn,
        )
        .unwrap();
    assert!(
        matches!(result, query::AccessResult::Denied),
        "With default_deny=true, user present, and no access ref, expected Denied, got: {result:?}"
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

    let def = registry.get_global("site_settings").unwrap().clone();

    let conn = pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&runner, &conn, None, None);
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

    let def = registry.get_global("site_settings").unwrap().clone();

    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
    let ctx = ServiceContext::global("site_settings", &def)
        .conn(&tx)
        .write_hooks(&wh)
        .user(Some(&user_a))
        .build();

    let mut data = DocumentFields::new();
    data.insert("site_name".to_string(), json!("Hacked"));
    let input = WriteInput::builder(data).build();

    let err = update_global_in_conn(&ctx, input)
        .expect_err("Constrained on global update must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("site_settings"), "got: {msg}");
    assert!(msg.contains("filter table"), "got: {msg}");
}

/// Regression: the collection `create` / `update` access function must receive
/// the incoming write data as `ctx.data`. It was passed `None`, so a data-gating
/// access rule silently couldn't see what was being written — despite the docs
/// and the in-code comment telling operators to gate on `ctx.data`.
#[test]
fn create_access_receives_incoming_data() {
    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("posts.lua"),
        r#"
crap.collections.define("posts", {
    fields = {
        { name = "title", type = "text" },
        { name = "flag", type = "text" },
    },
    access = { create = "hooks.gate.requires_flag" },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("gate.lua"),
        r#"
local M = {}

-- Allow the write only when the incoming data carries flag == "ok".
function M.requires_flag(ctx)
    return ctx.data ~= nil and ctx.data.flag == "ok"
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let def = registry.get_collection("posts").unwrap().clone();

    let create = |flag: &str| {
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&tx)
            .write_hooks(&wh)
            .build();
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Hello"));
        data.insert("flag".to_string(), json!(flag));
        let res = create_document_in_conn(&ctx, WriteInput::builder(data).build());
        if res.is_ok() {
            tx.commit().unwrap();
        }
        res
    };

    // flag == "ok" → the access fn reads ctx.data.flag and allows.
    assert!(
        create("ok").is_ok(),
        "create with flag=ok must be allowed (access fn could read ctx.data)"
    );

    // flag != "ok" → denied, which is only possible if the access fn saw ctx.data.
    let err = create("no").expect_err("create with flag=no must be denied via ctx.data");
    assert!(
        matches!(err, crap_cms::service::ServiceError::AccessDenied(_)),
        "expected AccessDenied, got: {err:?}"
    );
}

/// Regression: `before_change` on an update must expose `ctx.document_id` so a
/// hook can fetch the *pre-write* document on demand via
/// `crap.collections.find_by_id(ctx.collection, ctx.document_id)`. The update
/// write path previously never set `document_id` (and the incoming `data` need
/// not carry `id`), so the canonical "price may only increase" guard — which
/// must compare against the persisted old value — was impossible in a
/// before-hook. This locks the on-demand-original contract for the freeze.
#[test]
fn before_change_update_exposes_document_id_for_on_demand_original() {
    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("products.lua"),
        r#"
crap.collections.define("products", {
    fields = {
        { name = "price", type = "number" },
    },
    hooks = {
        before_change = { "hooks.guard.price_increase_only" },
    },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("guard.lua"),
        r#"
local M = {}

-- Fetch the pre-write document on demand and reject a price decrease.
-- Only meaningful on update; create has no prior document.
function M.price_increase_only(ctx)
    if ctx.operation ~= "update" then
        return ctx
    end
    if ctx.document_id == nil then
        error("document_id missing in before_change update")
    end
    local old = crap.collections.find_by_id(ctx.collection, ctx.document_id, { overrideAccess = true })
    if old ~= nil and ctx.data.price ~= nil and ctx.data.price < old.price then
        error("price may only increase")
    end
    return ctx
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let def = registry.get_collection("products").unwrap().clone();

    // Seed a product at price 10.
    let id = {
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
        let ctx = ServiceContext::collection("products", &def)
            .conn(&tx)
            .write_hooks(&wh)
            .build();
        let mut data = DocumentFields::new();
        data.insert("price".to_string(), json!(10));
        let (doc, _) = create_document_in_conn(&ctx, WriteInput::builder(data).build()).unwrap();
        tx.commit().unwrap();
        doc.id.to_string()
    };

    let update = |price: i64| {
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
        let ctx = ServiceContext::collection("products", &def)
            .conn(&tx)
            .write_hooks(&wh)
            .build();
        let mut data = DocumentFields::new();
        data.insert("price".to_string(), json!(price));
        let res = update_document(&ctx, &id, WriteInput::builder(data).build());
        if res.is_ok() {
            tx.commit().unwrap();
        }
        res
    };

    // Increase → allowed (the hook fetched old=10 via document_id and saw 20 >= 10).
    assert!(
        update(20).is_ok(),
        "price increase must be allowed (on-demand original resolved)"
    );

    // Decrease → rejected. Only possible if the hook resolved the persisted old
    // value through ctx.document_id; a missing document_id would have errored or
    // returned nil and let the decrease through.
    let err = update(5).expect_err("price decrease must be rejected via on-demand original");
    assert!(
        err.to_string().contains("price may only increase"),
        "expected the guard's rejection (proving the old value was resolved), got: {err:?}"
    );
}

/// Regression: `ctx.context` is scoped to a single *write operation* (one
/// create/update/delete lifecycle), NOT the whole request. Two updates in the
/// same transaction/request must each start with a fresh, empty `context` —
/// the docs previously called it "request-scoped", which would imply a leak
/// between operations (and across documents in a bulk op). Within one operation,
/// before→after sharing is covered by `context_flows_through_hooks`.
#[test]
fn hook_context_is_per_operation_not_per_request() {
    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("notes.lua"),
        r#"
crap.collections.define("notes", {
    fields = {
        { name = "body", type = "text" },
    },
    hooks = {
        before_change = { "hooks.scope.fresh_context" },
    },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("scope.lua"),
        r#"
local M = {}

-- Each write operation must begin with an empty context. If a previous
-- operation's marker is visible here, context is leaking across operations.
function M.fresh_context(ctx)
    if ctx.context.touched ~= nil then
        error("context bled across operations")
    end
    ctx.context.touched = true
    return ctx
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let def = registry.get_collection("notes").unwrap().clone();

    let make = |body: &str| -> String {
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
        let ctx = ServiceContext::collection("notes", &def)
            .conn(&tx)
            .write_hooks(&wh)
            .build();
        let mut data = DocumentFields::new();
        data.insert("body".to_string(), json!(body));
        let (doc, _) = create_document_in_conn(&ctx, WriteInput::builder(data).build()).unwrap();
        tx.commit().unwrap();
        doc.id.to_string()
    };

    let id_a = make("a");
    let id_b = make("b");

    // Two updates in ONE transaction/request. If context were request-scoped,
    // the second update's before_change would see the first's `touched` marker
    // and error.
    let mut conn = db_pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);

    let ctx_a = ServiceContext::collection("notes", &def)
        .conn(&tx)
        .write_hooks(&wh)
        .build();
    let mut d1 = DocumentFields::new();
    d1.insert("body".to_string(), json!("a2"));
    update_document(&ctx_a, &id_a, WriteInput::builder(d1).build()).expect("update A");

    let ctx_b = ServiceContext::collection("notes", &def)
        .conn(&tx)
        .write_hooks(&wh)
        .build();
    let mut d2 = DocumentFields::new();
    d2.insert("body".to_string(), json!("b2"));
    update_document(&ctx_b, &id_b, WriteInput::builder(d2).build())
        .expect("second update in same request must start with a fresh, empty context");

    tx.commit().unwrap();
}

/// Regression (#11): `after_change` hooks must see hydrated array/blocks/has-many
/// data, not just scalar columns. Hydration previously ran AFTER the after-change
/// hooks, so a hook reading `ctx.data.<array_field>` got `nil`. Hydration now runs
/// before `after_change` on every write path.
#[test]
fn after_change_sees_hydrated_array_data() {
    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("carts.lua"),
        r#"
crap.collections.define("carts", {
    fields = {
        { name = "items", type = "array", fields = {
            { name = "sku", type = "text" },
        } },
    },
    hooks = {
        after_change = { "hooks.cart.require_items" },
    },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("cart.lua"),
        r#"
local M = {}

-- after_change must see the array rows (hydrated), not nil.
function M.require_items(ctx)
    if type(ctx.data.items) ~= "table" then
        error("after_change did not see array 'items' (got " .. type(ctx.data.items) .. ")")
    end
    if #ctx.data.items ~= 2 then
        error("after_change saw " .. tostring(#ctx.data.items) .. " items, expected 2")
    end
    return ctx
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let def = registry.get_collection("carts").unwrap().clone();

    let mut conn = db_pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
    let ctx = ServiceContext::collection("carts", &def)
        .conn(&tx)
        .write_hooks(&wh)
        .build();
    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{ "sku": "a" }, { "sku": "b" }]));

    // If after_change couldn't see the hydrated array, the hook errors → create fails.
    create_document_in_conn(&ctx, WriteInput::builder(data).build())
        .expect("create must succeed — after_change must see the hydrated array rows");
    tx.commit().unwrap();

    // The UPDATE path must also hydrate join data before after_change (same
    // reorder as create). Driven via Lua since `update_document_in_conn` is
    // crate-private; it routes through the same service update path.
    let conn2 = db_pool.get().unwrap();
    let updated = runner
        .eval_lua_with_conn(
            r#"
        local found = crap.collections.find("carts", {})
        local id = found.documents[1].id
        crap.collections.update("carts", id, { items = { { sku = "c" }, { sku = "d" } } })
        return "ok"
        "#,
            &conn2,
            None,
        )
        .expect("update must succeed — after_change must see the hydrated array rows on update");
    assert_eq!(updated, "ok");
}

/// Regression (#8): access functions receive `ctx.operation`, so a single shared
/// access function can branch on the operation (create/update/delete) instead of
/// needing a separate function per operation.
#[test]
fn access_fn_receives_operation() {
    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("memos.lua"),
        r#"
crap.collections.define("memos", {
    fields = {
        { name = "body", type = "text" },
    },
    -- One shared fn gating two operations; it must distinguish them via ctx.operation.
    access = {
        create = "hooks.op.create_only",
        update = "hooks.op.create_only",
    },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("op.lua"),
        r#"
local M = {}

-- Allow only on create; deny update — provable only if ctx.operation is set.
function M.create_only(ctx)
    return ctx.operation == "create"
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let def = registry.get_collection("memos").unwrap().clone();

    // create → operation == "create" → allowed
    let id = {
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
        let ctx = ServiceContext::collection("memos", &def)
            .conn(&tx)
            .write_hooks(&wh)
            .build();
        let mut data = DocumentFields::new();
        data.insert("body".to_string(), json!("hi"));
        let (doc, _) = create_document_in_conn(&ctx, WriteInput::builder(data).build())
            .expect("create must be allowed (ctx.operation == \"create\")");
        tx.commit().unwrap();
        doc.id.to_string()
    };

    // update → operation == "update" → the same fn now denies
    let mut conn = db_pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
    let ctx = ServiceContext::collection("memos", &def)
        .conn(&tx)
        .write_hooks(&wh)
        .build();
    let mut data = DocumentFields::new();
    data.insert("body".to_string(), json!("bye"));
    let err = update_document(&ctx, &id, WriteInput::builder(data).build())
        .expect_err("update must be denied — the shared fn saw ctx.operation == \"update\"");
    assert!(
        matches!(err, crap_cms::service::ServiceError::AccessDenied(_)),
        "expected AccessDenied, got: {err:?}"
    );
}

/// Conditional required (#12): a field with `required_when` is required only when
/// its predicate returns truthy for the document being validated — expressing
/// "field B is required when field A has a certain value" without the static
/// `required` boolean and without touching the (correct) skip-on-nil validator
/// behavior.
#[test]
fn required_when_enforces_conditional_requirement() {
    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("orders.lua"),
        r#"
crap.collections.define("orders", {
    fields = {
        { name = "product_type", type = "text" },
        { name = "shipping_address", type = "text", required_when = "hooks.cond.is_physical" },
    },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("cond.lua"),
        r#"
local M = {}

-- shipping_address is required only for physical products.
function M.is_physical(ctx)
    return ctx.data.product_type == "physical"
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let def = registry.get_collection("orders").unwrap().clone();

    let create = |product_type: &str, shipping: Option<&str>| {
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
        let ctx = ServiceContext::collection("orders", &def)
            .conn(&tx)
            .write_hooks(&wh)
            .build();
        let mut data = DocumentFields::new();
        data.insert("product_type".to_string(), json!(product_type));
        if let Some(s) = shipping {
            data.insert("shipping_address".to_string(), json!(s));
        }
        let res = create_document_in_conn(&ctx, WriteInput::builder(data).build());
        if res.is_ok() {
            tx.commit().unwrap();
        }
        res
    };

    // Digital product: predicate false → shipping_address not required.
    assert!(
        create("digital", None).is_ok(),
        "digital product must not require shipping_address"
    );

    // Physical product, no shipping → predicate true + absent → validation error.
    let err = create("physical", None)
        .expect_err("physical product without shipping_address must fail validation");
    assert!(
        matches!(err, crap_cms::service::ServiceError::Validation(_)),
        "expected a Validation error, got: {err:?}"
    );

    // Physical product with shipping → satisfied.
    assert!(
        create("physical", Some("123 Main St")).is_ok(),
        "physical product with shipping_address must pass"
    );
}

/// Conditional required inside Array/Blocks rows (#12.3 parity): a `required_when`
/// predicate on a sub-field is evaluated per row, seeing the row as `ctx.data`
/// and the full document as `ctx.document`. Previously sub-field `required_when`
/// was silently ignored (only static `required` was checked in rows).
#[test]
fn required_when_enforces_conditional_requirement_in_array_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("orders2.lua"),
        r#"
crap.collections.define("orders2", {
    versions = { drafts = true },
    fields = {
        { name = "mode", type = "text" },
        { name = "items", type = "array", fields = {
            { name = "kind", type = "text" },
            { name = "serial", type = "text", required_when = "hooks.cond2.needs_serial" },
        }},
    },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("cond2.lua"),
        r#"
local M = {}

-- A serial is required for device rows (row-level `ctx.data`), OR whenever the
-- whole order is in strict mode (top-level `ctx.document`, outside the row).
function M.needs_serial(ctx)
    return ctx.data.kind == "device" or ctx.document.mode == "strict"
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let def = registry.get_collection("orders2").unwrap().clone();

    let create = |mode: &str, row: serde_json::Value, draft: bool| {
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
        let ctx = ServiceContext::collection("orders2", &def)
            .conn(&tx)
            .write_hooks(&wh)
            .build();
        let mut data = DocumentFields::new();
        data.insert("mode".to_string(), json!(mode));
        data.insert("items".to_string(), json!([row]));
        let res = create_document_in_conn(&ctx, WriteInput::builder(data).draft(draft).build());
        if res.is_ok() {
            tx.commit().unwrap();
        }
        res
    };

    // Non-device row, non-strict order → serial not required.
    assert!(
        create("normal", json!({ "kind": "other" }), false).is_ok(),
        "non-device row in a non-strict order must not require serial"
    );

    // Device row, no serial → row-level predicate (ctx.data) fires → error.
    let err = create("normal", json!({ "kind": "device" }), false)
        .expect_err("device row without serial must fail validation");
    assert!(
        matches!(err, crap_cms::service::ServiceError::Validation(_)),
        "expected a Validation error, got: {err:?}"
    );

    // Strict order, non-device row, no serial → document-level predicate
    // (ctx.document.mode) fires from inside the row → error. This proves the
    // sub-field predicate sees the full document, not just the row.
    let err = create("strict", json!({ "kind": "other" }), false)
        .expect_err("strict order without serial must fail validation");
    assert!(
        matches!(err, crap_cms::service::ServiceError::Validation(_)),
        "expected a Validation error from the document-level condition, got: {err:?}"
    );

    // Same strict order saved as a DRAFT → required (incl. required_when) is
    // skipped, so it must succeed.
    assert!(
        create("strict", json!({ "kind": "other" }), true).is_ok(),
        "draft save must skip the conditional requirement"
    );

    // Device row with serial → satisfied.
    assert!(
        create(
            "normal",
            json!({ "kind": "device", "serial": "SN-1" }),
            false
        )
        .is_ok(),
        "device row with serial must pass"
    );
}

/// Regression: the Lua bulk path (`crap.collections.update_many` /
/// `delete_many`) must report the real write `operation` to the collection's
/// access function — not the `"find"` used internally to select candidate rows.
/// The shared `enforce_access` helper previously hardcoded `"find"`, so a
/// shared access fn that branches on `ctx.operation` mis-gated bulk writes.
#[test]
fn bulk_update_many_reports_update_operation_to_access_fn() {
    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("widgets.lua"),
        r#"
crap.collections.define("widgets", {
    access = { update = "hooks.gate.assert_update_op" },
    fields = {
        { name = "name", type = "text" },
        { name = "status", type = "text" },
    },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("gate.lua"),
        r#"
local M = {}

-- The update access fn must see operation "update" and the right collection,
-- even when invoked from the bulk path that selects rows with a find filter.
function M.assert_update_op(ctx)
    if ctx.operation ~= "update" then
        error("WRONG_OP:" .. tostring(ctx.operation))
    end
    if ctx.collection ~= "widgets" then
        error("WRONG_COLLECTION:" .. tostring(ctx.collection))
    end
    return true
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let conn = db_pool.get().unwrap();
    let result = runner
        .eval_lua_with_conn(
            r#"
        crap.collections.create("widgets", { name = "a", status = "draft" })
        crap.collections.create("widgets", { name = "b", status = "draft" })

        local r = crap.collections.update_many("widgets",
            { where = { status = "draft" } },
            { status = "live" }
        )
        if r.modified ~= 2 then return "WRONG_MODIFIED:" .. tostring(r.modified) end
        return "ok"
        "#,
            &conn,
            None,
        )
        .expect("bulk update_many should succeed — access fn must see operation \"update\"");

    assert_eq!(
        result, "ok",
        "update_many must report operation \"update\" to the access fn"
    );
}

/// Regression: a soft delete is a "trash" operation. The configured
/// `access.trash` function must see `ctx.operation == "trash"` (matching the
/// admin permission grid), not `"delete"` — so a function that branches on the
/// operation gates trash vs hard-delete correctly.
#[test]
fn soft_delete_reports_trash_operation_to_access_fn() {
    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("trashables.lua"),
        r#"
crap.collections.define("trashables", {
    soft_delete = true,
    access = { trash = "hooks.tgate.assert_trash" },
    fields = {
        { name = "name", type = "text" },
    },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("tgate.lua"),
        r#"
local M = {}

function M.assert_trash(ctx)
    if ctx.operation ~= "trash" then
        error("WRONG_OP:" .. tostring(ctx.operation))
    end
    return true
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let conn = db_pool.get().unwrap();
    let result = runner
        .eval_lua_with_conn(
            r#"
        -- Single-doc soft delete (service delete path).
        local doc = crap.collections.create("trashables", { name = "x" })
        crap.collections.delete("trashables", doc.id)

        -- Bulk soft delete (delete_many path) must also report "trash".
        crap.collections.create("trashables", { name = "y" })
        crap.collections.create("trashables", { name = "z" })
        local d = crap.collections.delete_many("trashables", {})
        if d.deleted ~= 2 then return "WRONG_DELETED:" .. tostring(d.deleted) end
        return "ok"
        "#,
            &conn,
            None,
        )
        .expect("soft delete should succeed — trash access fn must see operation \"trash\"");

    assert_eq!(
        result, "ok",
        "single + bulk soft delete must report operation \"trash\" to access.trash"
    );
}

/// Regression: a global read invokes its `access.read` function with
/// `operation == "get"` (matching the global's own `before_read`/`after_read`
/// hooks) — not `"find"`, which is collection-specific.
#[test]
fn global_read_reports_get_operation_to_access_fn() {
    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("settings.lua"),
        r#"
crap.globals.define("settings", {
    access = { read = "hooks.ggate.assert_get" },
    fields = {
        { name = "title", type = "text" },
    },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("ggate.lua"),
        r#"
local M = {}

function M.assert_get(ctx)
    if ctx.operation ~= "get" then
        error("WRONG_OP:" .. tostring(ctx.operation))
    end
    return true
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let conn = db_pool.get().unwrap();
    let result = runner
        .eval_lua_with_conn(
            r#"
        crap.globals.get("settings")
        return "ok"
        "#,
            &conn,
            None,
        )
        .expect("global read should succeed — access fn must see operation \"get\"");

    assert_eq!(
        result, "ok",
        "global read must report operation \"get\" to access.read"
    );
}

/// Regression: the pool read path (`RunnerReadHooks`) must pass `user` and the
/// content `locale` to `before_read` hooks. They were previously dropped (the
/// pool path built an empty context), so a `before_read` hook couldn't see who
/// was reading or in which locale — asymmetric with the inline Lua path.
#[test]
fn before_read_pool_path_receives_user_and_locale() {
    use crap_cms::service::ReadHooks;

    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(
        collections_dir.join("posts.lua"),
        r#"
crap.collections.define("posts", {
    fields = { { name = "title", type = "text" } },
    hooks = { before_read = { "hooks.guard.require_user_and_locale" } },
})
"#,
    )
    .unwrap();
    std::fs::write(
        hooks_dir.join("guard.lua"),
        r#"
local M = {}

function M.require_user_and_locale(ctx)
    if ctx.user == nil then error("before_read: missing user") end
    if ctx.locale == nil then error("before_read: missing locale") end
    return ctx
end

return M
"#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();
    let def = registry.get_collection("posts").unwrap().clone();
    let conn = db_pool.get().unwrap();
    let user = make_user_doc("admin-1", "admin");

    // With user + locale, the hook's guards both pass.
    let rh = RunnerReadHooks::new(&runner, &conn, Some(&user), None);
    assert!(
        rh.before_read(&def.hooks, "posts", "find", Some("en"))
            .is_ok(),
        "before_read must see ctx.user and ctx.locale on the pool path"
    );

    // No user → the hook errors (proves user reaches the pool-path context).
    let rh_nouser = RunnerReadHooks::new(&runner, &conn, None, None);
    assert!(
        rh_nouser
            .before_read(&def.hooks, "posts", "find", Some("en"))
            .is_err(),
        "before_read must error when ctx.user is missing"
    );

    // No locale → the hook errors (proves locale reaches the context).
    assert!(
        rh.before_read(&def.hooks, "posts", "find", None).is_err(),
        "before_read must error when ctx.locale is missing"
    );
}

/// Regression: a custom field `validate` function must see `ctx.user`.
/// Validation runs on its own freshly-acquired pool VM (separate from the
/// write-hook VM), so the user previously arrived as `nil` — making a rule like
/// "only an editor may set this field" impossible.
#[test]
fn validator_receives_user() {
    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(
        collections_dir.join("posts.lua"),
        r#"
crap.collections.define("posts", {
    fields = {
        { name = "title", type = "text", validate = "hooks.v.require_user" },
    },
})
"#,
    )
    .unwrap();
    std::fs::write(
        hooks_dir.join("v.lua"),
        r#"
local M = {}

-- Reject the write unless a user is present in the validation context.
function M.require_user(_value, ctx)
    if ctx.user == nil then return "must be authenticated" end
    return true
end

return M
"#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();
    let def = registry.get_collection("posts").unwrap().clone();
    let user = make_user_doc("u1", "admin");

    let create = |user_arg: Option<&Document>| {
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&tx)
            .write_hooks(&wh)
            .user(user_arg)
            .build();
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Hello"));
        let res = create_document_in_conn(&ctx, WriteInput::builder(data).build());
        if res.is_ok() {
            tx.commit().unwrap();
        }
        res
    };

    // With a user, the validator sees ctx.user and allows.
    assert!(
        create(Some(&user)).is_ok(),
        "validator must see ctx.user and allow the write"
    );

    // Without a user, the validator rejects (proves ctx.user reaches it).
    let err = create(None).expect_err("validator must reject when ctx.user is nil");
    assert!(
        matches!(err, crap_cms::service::ServiceError::Validation(_)),
        "expected a Validation error, got: {err:?}"
    );
}

/// Helper: seed a versioned article directly so we can drive list/restore.
/// Returns (`doc_id`, `first_version_id`).
fn seed_versioned_article(
    pool: &crap_cms::db::DbPool,
    registry: &std::sync::Arc<crap_cms::core::Registry>,
    author_id: &str,
    title: &str,
) -> (String, String) {
    let def = registry
        .get_collection("versioned_articles")
        .unwrap()
        .clone();

    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!(title));
    data.insert("author_id".to_string(), json!(author_id));

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

    let def = registry
        .get_collection("versioned_articles")
        .unwrap()
        .clone();

    // user_a (not the author) should be denied because own_rows enforces
    // { author_id = user_a } against the parent row, which doesn't match.
    let user_a = make_user_doc("user_a", "editor");
    let conn = pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&runner, &conn, None, None);
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
    let hooks_b = RunnerReadHooks::new(&runner, &conn, None, None);
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

    let def = registry
        .get_collection("versioned_articles")
        .unwrap()
        .clone();
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

    let def = registry
        .get_collection("versioned_articles")
        .unwrap()
        .clone();

    // Seed a valid live row so restore has a target.
    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!("Original"));
    data.insert("author_id".to_string(), json!("user_b"));
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

    let job_def = registry.get_job("constrained_job").unwrap().clone();

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
        priority: 0,
        queue_retries: None,
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
    registry: &std::sync::Arc<crap_cms::core::Registry>,
    author_id: &str,
    title: &str,
    status: &str,
) -> String {
    let def = registry
        .get_collection("versioned_articles")
        .unwrap()
        .clone();

    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!(title));
    data.insert("author_id".to_string(), json!(author_id));

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

    let def = registry
        .get_collection("versioned_articles")
        .unwrap()
        .clone();

    let user_a = make_user_doc("user_a", "editor");
    let conn = pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&runner, &conn, None, None);
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

    let def = registry
        .get_collection("versioned_articles")
        .unwrap()
        .clone();

    let user_a = make_user_doc("user_a", "editor");
    let conn = pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&runner, &conn, None, None);
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
/// Returns (`row_exists`, `deleted_at_is_set`).
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
        crap.collections.delete("articles", "{id}", {{ forceHardDelete = true }})
        return "OK"
        "#
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
        crap.collections.delete("articles", "{id}")
        return "OK"
        "#
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
        crap.collections.delete("articles", "{id}", {{ forceHardDelete = false }})
        return "OK"
        "#
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
        local r = crap.collections.list_versions("versioned_articles", "{id}")
        return "OK:" .. tostring(#r.docs)
        "#
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
            "versioned_articles", "{id}", {{ overrideAccess = true }}
        )
        return tostring(#r.docs)
        "#
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
        crap.collections.restore_version("versioned_articles", "{id}", "{version_id}")
        return "OK"
        "#
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
            "versioned_articles", "{id}", "{version_id}",
            {{ overrideAccess = true }}
        )
        return d.id
        "#
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&user_a))
        .expect("overrideAccess = true must bypass the access check");
    assert_eq!(result, id, "restored doc id must match parent doc id");
}

/// Regression (#15): a job access function receives the queued payload as
/// `ctx.data`, so it can gate on *what* is being queued, not only who is queuing.
/// (#8 already gave it `ctx.operation = "trigger"` and `ctx.collection = <slug>`.)
#[test]
fn job_access_fn_receives_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let jobs_dir = tmp.path().join("jobs");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        jobs_dir.join("email.lua"),
        r#"
local M = {}

function M.send(ctx) end

crap.jobs.define("send_email", {
    handler = "jobs.email.send",
    access = "hooks.jobgate.allowed_only",
})

return M
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("jobgate.lua"),
        r"
local M = {}

-- Gate on the queued payload, not just the user.
function M.allowed_only(ctx)
    return ctx.data ~= nil and ctx.data.allowed == true
end

return M
",
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let job_def = registry.get_job("send_email").unwrap().clone();

    let queue = |payload: &str| {
        let conn = db_pool.get().unwrap();
        let ctx = ServiceContext::slug_only("send_email")
            .conn(&conn)
            .runner(&runner)
            .build();
        let input = QueueJobInput {
            job_def: &job_def,
            data: Some(payload),
            scheduled_by: "test",
            priority: 0,
            queue_retries: None,
        };
        queue_job(&ctx, &input)
    };

    // The access fn only allows when the payload carries allowed == true.
    assert!(
        queue(r#"{"allowed":true}"#).is_ok(),
        "allowed payload must queue (access fn saw ctx.data)"
    );
    let err = queue(r#"{"allowed":false}"#)
        .expect_err("disallowed payload must be rejected via ctx.data");
    assert!(
        matches!(err, crap_cms::service::ServiceError::AccessDenied(_)),
        "expected AccessDenied, got: {err:?}"
    );
}

/// Regression (#12.1): custom field validators receive `ctx.operation` (and
/// `ctx.id` on update), so a validator can branch on create-vs-update — e.g. an
/// "immutable after create" field.
#[test]
fn validator_receives_operation() {
    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("things.lua"),
        r#"
crap.collections.define("things", {
    fields = {
        { name = "sku", type = "text", validate = "hooks.imm.immutable_sku" },
    },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("imm.lua"),
        r#"
local M = {}

-- sku may be set on create but never changed on update.
function M.immutable_sku(value, ctx)
    if ctx.operation == "update" then
        return "sku is immutable after creation"
    end
    return true
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let def = registry.get_collection("things").unwrap().clone();

    // create with sku → operation == "create" → allowed
    let id = {
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
        let ctx = ServiceContext::collection("things", &def)
            .conn(&tx)
            .write_hooks(&wh)
            .build();
        let mut data = DocumentFields::new();
        data.insert("sku".to_string(), json!("ABC-1"));
        let (doc, _) = create_document_in_conn(&ctx, WriteInput::builder(data).build())
            .expect("create with sku must pass (operation == \"create\")");
        tx.commit().unwrap();
        doc.id.to_string()
    };

    // update sku → operation == "update" → validator rejects
    let mut conn = db_pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
    let ctx = ServiceContext::collection("things", &def)
        .conn(&tx)
        .write_hooks(&wh)
        .build();
    let mut data = DocumentFields::new();
    data.insert("sku".to_string(), json!("ABC-2"));
    let err = update_document(&ctx, &id, WriteInput::builder(data).build())
        .expect_err("changing sku on update must be rejected via ctx.operation");
    assert!(
        err.to_string().contains("immutable"),
        "expected the immutable-sku validation message, got: {err:?}"
    );
}

/// Regression (#14): field hooks fire for sub-fields *inside array and blocks
/// rows* (and `has_any_field_hook` detects them). Previously the field-hook
/// walker never recursed into rows, so these hooks silently never ran.
#[test]
fn field_hooks_fire_inside_array_and_blocks_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("carts2.lua"),
        r#"
crap.collections.define("carts2", {
    fields = {
        { name = "items", type = "array", fields = {
            { name = "name", type = "text", hooks = { before_change = { "hooks.up.upper" } } },
        } },
        { name = "sections", type = "blocks", blocks = {
            { type = "text", fields = {
                { name = "heading", type = "text", hooks = { before_change = { "hooks.up.upper" } } },
            } },
        } },
    },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("up.lua"),
        r#"
local M = {}

function M.upper(value, ctx)
    if type(value) == "string" then
        return value:upper()
    end
    return value
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let def = registry.get_collection("carts2").unwrap().clone();

    let mut conn = db_pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
    let ctx = ServiceContext::collection("carts2", &def)
        .conn(&tx)
        .write_hooks(&wh)
        .build();
    let mut data = DocumentFields::new();
    data.insert(
        "items".to_string(),
        json!([{ "name": "foo" }, { "name": "bar" }]),
    );
    data.insert(
        "sections".to_string(),
        json!([{ "_block_type": "text", "heading": "hi" }]),
    );

    let (doc, _) = create_document_in_conn(&ctx, WriteInput::builder(data).build())
        .expect("create must succeed");
    tx.commit().unwrap();

    // Array sub-field hook fired per row.
    let items = doc
        .fields
        .get("items")
        .and_then(|v| v.as_array())
        .expect("items array");
    assert_eq!(items[0].get("name").and_then(|v| v.as_str()), Some("FOO"));
    assert_eq!(items[1].get("name").and_then(|v| v.as_str()), Some("BAR"));

    // Blocks sub-field hook fired per row, with `_block_type` preserved.
    let sections = doc
        .fields
        .get("sections")
        .and_then(|v| v.as_array())
        .expect("sections array");
    assert_eq!(
        sections[0].get("heading").and_then(|v| v.as_str()),
        Some("HI")
    );
    assert_eq!(
        sections[0].get("_block_type").and_then(|v| v.as_str()),
        Some("text"),
        "_block_type must be preserved on write-back"
    );
}

/// Regression (#12.2): a validator on a field *inside an array/blocks row* can
/// reach the full parent document via `ctx.document` (its own row stays as
/// `ctx.data`), enabling cross-row / parent cross-field rules.
#[test]
fn sub_field_validator_sees_parent_document() {
    let tmp = tempfile::tempdir().unwrap();
    let collections_dir = tmp.path().join("collections");
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&collections_dir).unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        collections_dir.join("invoices.lua"),
        r#"
crap.collections.define("invoices", {
    fields = {
        { name = "currency", type = "text" },
        { name = "lines", type = "array", fields = {
            { name = "amount", type = "text", validate = "hooks.inv.line_ok" },
        } },
    },
})
"#,
    )
    .unwrap();

    std::fs::write(
        hooks_dir.join("inv.lua"),
        r#"
local M = {}

-- A line is only valid when the parent invoice has a currency set. The line
-- itself (ctx.data) has no currency — only the parent document does.
function M.line_ok(value, ctx)
    if ctx.document.currency == nil or ctx.document.currency == "" then
        return "currency must be set on the invoice before adding lines"
    end
    return true
end

return M
"#,
    )
    .unwrap();

    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(tmp.path(), &config).unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let def = registry.get_collection("invoices").unwrap().clone();

    let create = |currency: Option<&str>| {
        let mut conn = db_pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let wh = RunnerWriteHooks::new(&runner).with_conn(&tx);
        let ctx = ServiceContext::collection("invoices", &def)
            .conn(&tx)
            .write_hooks(&wh)
            .build();
        let mut data = DocumentFields::new();
        if let Some(c) = currency {
            data.insert("currency".to_string(), json!(c));
        }
        data.insert("lines".to_string(), json!([{ "amount": "10" }]));
        let res = create_document_in_conn(&ctx, WriteInput::builder(data).build());
        if res.is_ok() {
            tx.commit().unwrap();
        }
        res
    };

    // currency set on the parent → the sub-field validator (reading ctx.document) passes.
    assert!(
        create(Some("USD")).is_ok(),
        "line valid when parent invoice has a currency"
    );

    // currency missing → the sub-field validator rejects via ctx.document.
    let err = create(None)
        .expect_err("line must be rejected — sub-field validator reads parent ctx.document");
    assert!(
        matches!(err, crap_cms::service::ServiceError::Validation(_)),
        "expected a Validation error, got: {err:?}"
    );
}
