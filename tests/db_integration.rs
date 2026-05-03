use std::collections::HashMap;

use crap_cms::config::CrapConfig;
use crap_cms::core::Registry;
use crap_cms::core::collection::{Auth, CollectionDefinition, GlobalDefinition, Labels};
use crap_cms::core::field::{FieldDefinition, FieldType, LocalizedString};
use crap_cms::db::{migrate, ops, pool, query};
use serde_json::json;

fn make_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    let title = FieldDefinition {
        name: "title".to_string(),
        required: true,
        ..Default::default()
    };
    let status = FieldDefinition {
        name: "status".to_string(),
        field_type: FieldType::Select,
        default_value: Some(json!("draft")),
        ..Default::default()
    };
    def.fields = vec![title, status];
    def
}

fn create_test_pool() -> (tempfile::TempDir, crap_cms::db::DbPool) {
    let tmp = tempfile::tempdir().expect("Failed to create temp dir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &config).expect("Failed to create pool");
    (tmp, db_pool)
}

#[test]
fn full_crud_cycle() {
    let (_tmp, pool) = create_test_pool();

    // Set up registry with posts collection
    let registry = Registry::shared();
    let def = make_posts_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }

    // Sync schema
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale)
        .expect("Failed to sync schema");

    // Create
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Hello World".to_string());
    data.insert("status".to_string(), "published".to_string());

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let doc = query::create(&tx, "posts", &def, &data, None).expect("Failed to create document");
    tx.commit().expect("Commit");

    assert_eq!(doc.get_str("title"), Some("Hello World"));
    assert_eq!(doc.get_str("status"), Some("published"));
    assert!(doc.created_at.is_some());
    let doc_id = doc.id.clone();

    // Read
    let found = ops::find_document_by_id(&pool, "posts", &def, &doc_id, None)
        .expect("Failed to find document")
        .expect("Document not found");
    assert_eq!(found.id, doc_id);
    assert_eq!(found.get_str("title"), Some("Hello World"));

    // List
    let all = ops::find_documents(&pool, "posts", &def, &query::FindQuery::default(), None)
        .expect("Failed to list documents");
    assert_eq!(all.len(), 1);

    // Update
    let mut update_data = HashMap::new();
    update_data.insert("title".to_string(), "Updated Title".to_string());
    update_data.insert("status".to_string(), "draft".to_string());

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let updated = query::update(&tx, "posts", &def, &doc_id, &update_data, None)
        .expect("Failed to update document");
    tx.commit().expect("Commit");
    assert_eq!(updated.get_str("title"), Some("Updated Title"));

    // Delete
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    query::delete(&tx, "posts", &doc_id).expect("Failed to delete document");
    tx.commit().expect("Commit");

    let deleted =
        ops::find_document_by_id(&pool, "posts", &def, &doc_id, None).expect("Query failed");
    assert!(deleted.is_none());
}

#[test]
fn sync_schema_adds_columns() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();

    // Start with one field
    let mut def = make_posts_def();
    def.fields = vec![def.fields[0].clone()]; // title only
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("First sync failed");

    // Add a field
    def.fields
        .push(FieldDefinition::builder("body", FieldType::Textarea).build());
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Second sync failed");

    // Verify we can use the new column
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    data.insert("body".to_string(), "Some body text".to_string());

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let doc = query::create(&tx, "posts", &def, &data, None).expect("Failed to create");
    tx.commit().expect("Commit");
    assert_eq!(doc.get_str("body"), Some("Some body text"));
}

#[test]
fn sync_schema_adds_timestamp_columns_to_existing_table() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();

    // Create a collection WITHOUT timestamps
    let mut def = make_posts_def();
    def.timestamps = false;
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("First sync");

    // Insert a row (no timestamp columns exist)
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Old post".to_string());
    {
        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        query::create(&tx, "posts", &def, &data, None).expect("Create without timestamps");
        tx.commit().unwrap();
    }

    // Now enable timestamps and re-sync — this should add created_at/updated_at via ALTER TABLE
    def.timestamps = true;
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale)
        .expect("Second sync with timestamps");

    // Verify we can query (the bug: SELECT ... created_at, updated_at would fail)
    let find_query = query::FindQuery::default();
    let conn = pool.get().unwrap();
    let docs = query::find(&conn, "posts", &def, &find_query, None)
        .expect("Find should succeed after adding timestamp columns");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("title"), Some("Old post"));

    // Existing row has NULL timestamps (added via ALTER TABLE with no default)
    assert!(docs[0].created_at.is_none());

    // New rows get timestamps set by the query layer
    drop(conn);
    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let mut data2 = HashMap::new();
    data2.insert("title".to_string(), "New post".to_string());
    let new_doc = query::create(&tx, "posts", &def, &data2, None).expect("Create with timestamps");
    tx.commit().unwrap();
    assert!(new_doc.created_at.is_some());
}

#[test]
fn count_documents() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_posts_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");

    // Insert 3 documents
    for i in 0..3 {
        let mut data = HashMap::new();
        data.insert("title".to_string(), format!("Post {}", i));
        let mut conn = pool.get().expect("DB connection");
        let tx = conn.transaction().expect("Start transaction");
        query::create(&tx, "posts", &def, &data, None).expect("Create failed");
        tx.commit().expect("Commit");
    }

    let total = ops::count_documents(&pool, "posts", &def, &[], None).expect("Count failed");
    assert_eq!(total, 3);
}

#[test]
fn filter_rejects_invalid_field_name() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_posts_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");

    let find_query = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "nonexistent".to_string(),
            op: query::FilterOp::Equals("test".to_string()),
        })])
        .build();

    let result = ops::find_documents(&pool, "posts", &def, &find_query, None);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Invalid field 'nonexistent'")
    );
}

#[test]
fn order_by_rejects_invalid_field_name() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_posts_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");

    let find_query = query::FindQuery::builder()
        .order_by(Some("nonexistent".to_string()))
        .build();

    let result = ops::find_documents(&pool, "posts", &def, &find_query, None);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Invalid field 'nonexistent'")
    );
}

#[test]
fn sql_injection_in_filter_field_blocked() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_posts_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");

    let find_query = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "1=1; DROP TABLE posts; --".to_string(),
            op: query::FilterOp::Equals("x".to_string()),
        })])
        .build();

    let result = ops::find_documents(&pool, "posts", &def, &find_query, None);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Invalid field"),
        "Expected invalid field error, got: {}",
        err_msg
    );
}

// ── Seed helper (used by type coercion and count_where_field_eq tests) ─────────

/// Set up a fresh DB with 5 seeded posts for filter testing.
/// Returns (pool, def, _tmp). Hold _tmp to keep the temp dir alive.
fn seed_posts() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    CollectionDefinition,
) {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_posts_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");

    // Status values: "" means pass empty string → coerce_value converts to NULL.
    // Omitting the field entirely would use the column DEFAULT ('draft'), not NULL.
    let rows: Vec<(&str, &str)> = vec![
        ("Alpha Post", "published"),
        ("Beta Post", "draft"),
        ("Gamma Post", "published"),
        ("Delta Post", "archived"),
        ("Epsilon Post", ""), // empty string → NULL via coerce_value
    ];

    for (title, status) in &rows {
        let mut data = HashMap::new();
        data.insert("title".to_string(), title.to_string());
        data.insert("status".to_string(), status.to_string());
        let mut conn = pool.get().expect("DB connection");
        let tx = conn.transaction().expect("Start transaction");
        query::create(&tx, "posts", &def, &data, None).expect("Create failed");
        tx.commit().expect("Commit");
    }

    (_tmp, pool, def)
}

// ── Helper: auth collection definition ────────────────────────────────────────

fn make_users_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("users");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("User".to_string())),
        plural: Some(LocalizedString::Plain("Users".to_string())),
    };
    def.timestamps = true;
    let email = FieldDefinition {
        name: "email".to_string(),
        field_type: FieldType::Email,
        required: true,
        unique: true,
        ..Default::default()
    };
    let name = FieldDefinition {
        name: "name".to_string(),
        ..Default::default()
    };
    def.fields = vec![email, name];
    def.auth = Some(Auth {
        enabled: true,
        verify_email: true,
        ..Auth::default()
    });
    def
}

fn setup_auth_collection() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    CollectionDefinition,
) {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_users_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");

    // Insert a test user
    let mut data = HashMap::new();
    data.insert("email".to_string(), "alice@example.com".to_string());
    data.insert("name".to_string(), "Alice".to_string());
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    query::create(&tx, "users", &def, &data, None).expect("Create user failed");
    tx.commit().expect("Commit");

    (_tmp, pool, def)
}

// ── 1A. Auth Query Functions ──────────────────────────────────────────────────

#[test]
fn find_by_email_returns_user() {
    let (_tmp, pool, def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");
    let result =
        query::find_by_email(&conn, "users", &def, "alice@example.com").expect("Query failed");
    assert!(result.is_some());
    let doc = result.unwrap();
    assert_eq!(doc.get_str("email"), Some("alice@example.com"));
    assert_eq!(doc.get_str("name"), Some("Alice"));
}

#[test]
fn find_by_email_missing_returns_none() {
    let (_tmp, pool, def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");
    let result = query::find_by_email(&conn, "users", &def, "nonexistent@example.com")
        .expect("Query failed");
    assert!(result.is_none());
}

#[test]
fn update_password_and_get_hash() {
    let (_tmp, pool, def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");

    let user = query::find_by_email(&conn, "users", &def, "alice@example.com")
        .expect("Query failed")
        .expect("User not found");

    // Initially no password hash
    let hash = query::get_password_hash(&conn, "users", &user.id).expect("Get hash failed");
    assert!(hash.is_none());

    // Update password
    query::update_password(&conn, "users", &user.id, "secret123").expect("Update password failed");

    // Verify hash is now set
    let hash = query::get_password_hash(&conn, "users", &user.id).expect("Get hash failed");
    assert!(hash.is_some());
    let hash_str = hash.unwrap();
    assert!(hash_str.as_ref().starts_with("$argon2"));
}

#[test]
fn get_password_hash_missing_user() {
    let (_tmp, pool, _def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");
    let result = query::get_password_hash(&conn, "users", "nonexistent-id").expect("Query failed");
    assert!(result.is_none());
}

#[test]
fn set_and_find_reset_token() {
    let (_tmp, pool, def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");

    let user = query::find_by_email(&conn, "users", &def, "alice@example.com")
        .expect("Query failed")
        .expect("User not found");

    let exp = chrono::Utc::now().timestamp() + 3600;
    query::set_reset_token(&conn, "users", &user.id, "reset-token-abc", exp)
        .expect("Set reset token failed");

    let found = query::find_by_reset_token(&conn, "users", &def, "reset-token-abc")
        .expect("Find by reset token failed");
    assert!(found.is_some());
    let (doc, token_exp) = found.unwrap();
    assert_eq!(doc.id, user.id);
    assert_eq!(token_exp, exp);
}

#[test]
fn find_reset_token_wrong_token() {
    let (_tmp, pool, def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");
    let result =
        query::find_by_reset_token(&conn, "users", &def, "wrong-token").expect("Query failed");
    assert!(result.is_none());
}

#[test]
fn clear_reset_token() {
    let (_tmp, pool, def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");

    let user = query::find_by_email(&conn, "users", &def, "alice@example.com")
        .expect("Query failed")
        .expect("User not found");

    let exp = chrono::Utc::now().timestamp() + 3600;
    query::set_reset_token(&conn, "users", &user.id, "token-to-clear", exp).expect("Set failed");

    query::clear_reset_token(&conn, "users", &user.id).expect("Clear failed");

    let found =
        query::find_by_reset_token(&conn, "users", &def, "token-to-clear").expect("Query failed");
    assert!(found.is_none());
}

#[test]
fn set_and_find_verification_token() {
    let (_tmp, pool, def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");

    let user = query::find_by_email(&conn, "users", &def, "alice@example.com")
        .expect("Query failed")
        .expect("User not found");

    query::set_verification_token(&conn, "users", &user.id, "verify-abc", 9999999999)
        .expect("Set verification token failed");

    let found =
        query::find_by_verification_token(&conn, "users", &def, "verify-abc").expect("Find failed");
    assert!(found.is_some());
    let (doc, exp) = found.unwrap();
    assert_eq!(doc.id, user.id);
    assert_eq!(exp, 9999999999);
}

#[test]
fn find_verification_token_wrong() {
    let (_tmp, pool, def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");
    let result = query::find_by_verification_token(&conn, "users", &def, "wrong-verify")
        .expect("Query failed");
    assert!(result.is_none());
}

#[test]
fn mark_verified_and_check() {
    let (_tmp, pool, def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");

    let user = query::find_by_email(&conn, "users", &def, "alice@example.com")
        .expect("Query failed")
        .expect("User not found");

    // Initially not verified
    let verified = query::is_verified(&conn, "users", &user.id).expect("Check failed");
    assert!(!verified);

    // Mark verified
    query::mark_verified(&conn, "users", &user.id).expect("Mark verified failed");

    // Now verified
    let verified = query::is_verified(&conn, "users", &user.id).expect("Check failed");
    assert!(verified);
}

#[test]
fn is_verified_default_false() {
    let (_tmp, pool, def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");

    let user = query::find_by_email(&conn, "users", &def, "alice@example.com")
        .expect("Query failed")
        .expect("User not found");

    let verified = query::is_verified(&conn, "users", &user.id).expect("Check failed");
    assert!(!verified);
}

#[test]
fn count_where_field_eq_basic() {
    let (_tmp, pool, _def) = seed_posts();
    let conn = pool.get().expect("DB connection");
    let count = query::count_where_field_eq(&conn, "posts", "status", "published", None, false)
        .expect("Count failed");
    assert_eq!(count, 2);
}

#[test]
fn count_where_field_eq_with_exclude() {
    let (_tmp, pool, def) = seed_posts();
    let conn = pool.get().expect("DB connection");

    // Find one published doc to exclude
    let fq = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::Equals("published".to_string()),
        })])
        .build();
    let docs = ops::find_documents(&pool, "posts", &def, &fq, None).expect("Find failed");
    assert!(!docs.is_empty());
    let exclude_id = &docs[0].id;

    let count = query::count_where_field_eq(
        &conn,
        "posts",
        "status",
        "published",
        Some(exclude_id),
        false,
    )
    .expect("Count failed");
    assert_eq!(count, 1);
}

// ── 1B. Globals ───────────────────────────────────────────────────────────────

fn make_global_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("site_settings");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Site Settings".to_string())),
        plural: None,
    };
    let site_name = FieldDefinition {
        name: "site_name".to_string(),
        ..Default::default()
    };
    let tagline = FieldDefinition {
        name: "tagline".to_string(),
        ..Default::default()
    };
    def.fields = vec![site_name, tagline];
    def
}

fn setup_global() -> (tempfile::TempDir, crap_cms::db::DbPool, GlobalDefinition) {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_global_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_global(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");
    (_tmp, pool, def)
}

#[test]
fn global_default_row_exists_after_sync() {
    let (_tmp, pool, def) = setup_global();
    let conn = pool.get().expect("DB connection");
    let doc = query::get_global(&conn, "site_settings", &def, None).expect("Get global failed");
    assert_eq!(doc.id, "default");
}

#[test]
fn get_global_returns_default() {
    let (_tmp, pool, def) = setup_global();
    let conn = pool.get().expect("DB connection");
    let doc = query::get_global(&conn, "site_settings", &def, None).expect("Get global failed");
    assert_eq!(doc.id, "default");
    // Fields should be null/empty initially
    assert!(
        doc.get_str("site_name").is_none()
            || doc.get("site_name") == Some(&serde_json::Value::Null)
    );
}

#[test]
fn update_global_and_read_back() {
    let (_tmp, pool, def) = setup_global();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    let mut data = HashMap::new();
    data.insert("site_name".to_string(), "My CMS".to_string());
    data.insert("tagline".to_string(), "The best CMS".to_string());
    let doc = query::update_global(&tx, "site_settings", &def, &data, None)
        .expect("Update global failed");
    tx.commit().expect("Commit");

    assert_eq!(doc.get_str("site_name"), Some("My CMS"));
    assert_eq!(doc.get_str("tagline"), Some("The best CMS"));

    // Read back
    let conn = pool.get().expect("DB connection");
    let doc2 = query::get_global(&conn, "site_settings", &def, None).expect("Get global failed");
    assert_eq!(doc2.get_str("site_name"), Some("My CMS"));
    assert_eq!(doc2.get_str("tagline"), Some("The best CMS"));
}

#[test]
fn update_global_preserves_unset_fields() {
    let (_tmp, pool, def) = setup_global();

    // First update: set both fields
    {
        let mut conn = pool.get().expect("DB connection");
        let tx = conn.transaction().expect("Start transaction");
        let mut data = HashMap::new();
        data.insert("site_name".to_string(), "Original Name".to_string());
        data.insert("tagline".to_string(), "Original Tagline".to_string());
        query::update_global(&tx, "site_settings", &def, &data, None).expect("Update failed");
        tx.commit().expect("Commit");
    }

    // Second update: only set site_name
    {
        let mut conn = pool.get().expect("DB connection");
        let tx = conn.transaction().expect("Start transaction");
        let mut data = HashMap::new();
        data.insert("site_name".to_string(), "New Name".to_string());
        query::update_global(&tx, "site_settings", &def, &data, None).expect("Update failed");
        tx.commit().expect("Commit");
    }

    // Tagline should still be the original
    let conn = pool.get().expect("DB connection");
    let doc = query::get_global(&conn, "site_settings", &def, None).expect("Get global failed");
    assert_eq!(doc.get_str("site_name"), Some("New Name"));
    assert_eq!(doc.get_str("tagline"), Some("Original Tagline"));
}

// ── Helper: make_field (used by type coercion, group, and migration tests) ───

fn make_field(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, field_type).build()
}

// ── 1E. Type Coercion & Edge Cases ────────────────────────────────────────────

#[test]
fn coerce_checkbox_values() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let mut def = CollectionDefinition::new("forms");
    def.timestamps = true;
    def.fields = vec![make_field("active", FieldType::Checkbox)];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync");

    // "on" → 1
    let mut data = HashMap::new();
    data.insert("active".to_string(), "on".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "forms", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");
    assert_eq!(doc.get("active").unwrap().as_i64(), Some(1));

    // "false" → 0
    let mut data = HashMap::new();
    data.insert("active".to_string(), "false".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "forms", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");
    assert_eq!(doc.get("active").unwrap().as_i64(), Some(0));
}

#[test]
fn coerce_number_valid() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let mut def = CollectionDefinition::new("metrics");
    def.timestamps = true;
    def.fields = vec![make_field("score", FieldType::Number)];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync");

    let mut data = HashMap::new();
    data.insert("score".to_string(), "42.5".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "metrics", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");
    assert_eq!(doc.get("score").unwrap().as_f64(), Some(42.5));
}
