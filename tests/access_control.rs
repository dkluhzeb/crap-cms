use std::collections::HashMap;
use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::Document;
use crap_cms::db::{migrate, ops, pool, query};
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;

fn setup() -> (tempfile::TempDir, crap_cms::db::DbPool, crap_cms::core::SharedRegistry, HookRunner) {
    let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("example");
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();

    // Sync schema so tables exist
    migrate::sync_all(&db_pool, &registry).unwrap();

    let runner = HookRunner::new(&config_dir, registry.clone(), &config).unwrap();
    (tmp, db_pool, registry, runner)
}

fn make_user_doc(id: &str, role: &str) -> Document {
    let mut doc = Document::new(id.to_string());
    doc.fields.insert("role".into(), serde_json::json!(role));
    doc.fields.insert("email".into(), serde_json::json!(format!("{}@test.com", role)));
    doc.fields.insert("name".into(), serde_json::json!(role.to_uppercase()));
    doc
}

// ── 1. Lua Parsing ──────────────────────────────────────────────────────────

#[test]
fn access_config_parsed_from_lua() {
    let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("example");
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).unwrap();
    let reg = registry.read().unwrap();
    let posts = reg.get_collection("posts").expect("posts collection not found");

    assert_eq!(posts.access.read.as_deref(), Some("hooks.access.public_read"));
    assert_eq!(posts.access.create.as_deref(), Some("hooks.access.authenticated"));
    assert_eq!(posts.access.update.as_deref(), Some("hooks.access.authenticated"));
    assert_eq!(posts.access.delete.as_deref(), Some("hooks.access.admin_only"));
}

#[test]
fn field_access_parsed_from_lua() {
    let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("example");
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).unwrap();
    let reg = registry.read().unwrap();
    let posts = reg.get_collection("posts").expect("posts collection not found");

    let status_field = posts.fields.iter().find(|f| f.name == "status")
        .expect("status field not found");
    assert_eq!(status_field.access.update.as_deref(), Some("hooks.access.admin_only"));
    assert!(status_field.access.read.is_none());
    assert!(status_field.access.create.is_none());
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
fn public_read_allows_anonymous() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();

    let result = runner.check_access(
        Some("hooks.access.public_read"), None, None, None, &conn,
    ).unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));
}

#[test]
fn authenticated_denies_anonymous() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();

    let result = runner.check_access(
        Some("hooks.access.authenticated"), None, None, None, &conn,
    ).unwrap();
    assert!(matches!(result, query::AccessResult::Denied));
}

#[test]
fn authenticated_allows_user() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();
    let editor = make_user_doc("editor-1", "editor");

    let result = runner.check_access(
        Some("hooks.access.authenticated"), Some(&editor), None, None, &conn,
    ).unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));
}

#[test]
fn admin_only_denies_editor() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();
    let editor = make_user_doc("editor-1", "editor");

    let result = runner.check_access(
        Some("hooks.access.admin_only"), Some(&editor), None, None, &conn,
    ).unwrap();
    assert!(matches!(result, query::AccessResult::Denied));
}

#[test]
fn admin_only_allows_admin() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();
    let admin = make_user_doc("admin-1", "admin");

    let result = runner.check_access(
        Some("hooks.access.admin_only"), Some(&admin), None, None, &conn,
    ).unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));
}

#[test]
fn own_or_admin_denies_anonymous() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();

    let result = runner.check_access(
        Some("hooks.access.own_or_admin"), None, None, None, &conn,
    ).unwrap();
    assert!(matches!(result, query::AccessResult::Denied));
}

#[test]
fn own_or_admin_allows_admin() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();
    let admin = make_user_doc("admin-1", "admin");

    let result = runner.check_access(
        Some("hooks.access.own_or_admin"), Some(&admin), None, None, &conn,
    ).unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));
}

#[test]
fn own_or_admin_constrains_editor() {
    let (_tmp, pool, _registry, runner) = setup();
    let conn = pool.get().unwrap();
    let editor = make_user_doc("editor-1", "editor");

    let result = runner.check_access(
        Some("hooks.access.own_or_admin"), Some(&editor), None, None, &conn,
    ).unwrap();

    match result {
        query::AccessResult::Constrained(clauses) => {
            assert_eq!(clauses.len(), 1);
            match &clauses[0] {
                query::FilterClause::Single(f) => {
                    assert_eq!(f.field, "created_by");
                    match &f.op {
                        query::FilterOp::Equals(val) => assert_eq!(val, "editor-1"),
                        other => panic!("Expected Equals op, got {:?}", other),
                    }
                }
                other => panic!("Expected Single clause, got {:?}", other),
            }
        }
        other => panic!("Expected Constrained, got {:?}", other),
    }
}

// ── 3. Field-Level Access ───────────────────────────────────────────────────

#[test]
fn field_write_strips_for_editor() {
    let (_tmp, pool, registry, runner) = setup();
    let conn = pool.get().unwrap();
    let editor = make_user_doc("editor-1", "editor");

    let reg = registry.read().unwrap();
    let posts = reg.get_collection("posts").unwrap();

    let denied = runner.check_field_write_access(
        &posts.fields, Some(&editor), "update", &conn,
    );
    // status field has access.update = admin_only, editor is not admin
    assert!(denied.contains(&"status".to_string()),
        "Expected 'status' to be denied for editor, got: {:?}", denied);
}

#[test]
fn field_write_allows_for_admin() {
    let (_tmp, pool, registry, runner) = setup();
    let conn = pool.get().unwrap();
    let admin = make_user_doc("admin-1", "admin");

    let reg = registry.read().unwrap();
    let posts = reg.get_collection("posts").unwrap();

    let denied = runner.check_field_write_access(
        &posts.fields, Some(&admin), "update", &conn,
    );
    // admin should be allowed to update all fields
    assert!(!denied.contains(&"status".to_string()),
        "Expected 'status' to be allowed for admin, got denied: {:?}", denied);
}

#[test]
fn field_read_no_config_allows_all() {
    let (_tmp, pool, registry, runner) = setup();
    let conn = pool.get().unwrap();

    let reg = registry.read().unwrap();
    let posts = reg.get_collection("posts").unwrap();

    // No field has read access configured, so nothing should be denied
    let denied = runner.check_field_read_access(&posts.fields, None, &conn);
    assert!(denied.is_empty(),
        "Expected no denied fields for read, got: {:?}", denied);
}

// ── 4. End-to-End with DB ───────────────────────────────────────────────────

#[test]
fn constrained_find_filters_results() {
    let (_tmp, pool, registry, _runner) = setup();

    let reg = registry.read().unwrap();
    let posts = reg.get_collection("posts").unwrap().clone();
    drop(reg);

    // Create posts with different status values
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
        data.insert("status".to_string(), status.to_string());

        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        query::create(&tx, "posts", &posts, &data).unwrap();
        tx.commit().unwrap();
    }

    // Simulate a Constrained access result (like own_or_admin would return),
    // but using the `status` field which exists on the posts collection.
    let constraint_filters = vec![
        query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::Equals("draft".to_string()),
        }),
    ];

    let find_query = query::FindQuery {
        filters: constraint_filters,
        ..Default::default()
    };

    let docs = ops::find_documents(&pool, "posts", &posts, &find_query).unwrap();

    // Should only see draft posts
    assert_eq!(docs.len(), 2, "Expected 2 draft posts, got {}", docs.len());
    for doc in &docs {
        assert_eq!(
            doc.get_str("status"), Some("draft"),
            "Expected all docs to have status=draft"
        );
    }
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
        data.insert("status".to_string(), "draft".to_string());

        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        query::create(&tx, "posts", &posts, &data).unwrap();
        tx.commit().unwrap();
    }

    // Verify public_read (Allowed) returns all posts
    let conn = pool.get().unwrap();
    let result = runner.check_access(
        posts.access.read.as_deref(), None, None, None, &conn,
    ).unwrap();
    assert!(matches!(result, query::AccessResult::Allowed));

    let all_docs = ops::find_documents(&pool, "posts", &posts, &query::FindQuery::default()).unwrap();
    assert_eq!(all_docs.len(), 2);

    // Verify admin_only (Denied) blocks anonymous delete
    let result = runner.check_access(
        posts.access.delete.as_deref(), None, None, None, &conn,
    ).unwrap();
    assert!(matches!(result, query::AccessResult::Denied));
}
