use std::collections::HashMap;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::Registry;
use crap_cms::core::collection::{Auth, CollectionDefinition, GlobalDefinition, Labels};
use crap_cms::core::field::{
    BlockDefinition, FieldDefinition, FieldType, LocalizedString, RelationshipConfig,
};
use crap_cms::db::{migrate, ops, pool, query};
use serde_json::json;

fn make_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("status", FieldType::Select)
            .default_value(json!("draft"))
            .build(),
    ];
    def
}

fn create_test_pool() -> (tempfile::TempDir, crap_cms::db::DbPool) {
    let tmp = tempfile::tempdir().expect("Failed to create temp dir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &config).expect("Failed to create pool");
    (tmp, db_pool)
}

fn make_field(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, field_type).build()
}

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

    let rows: Vec<(&str, &str)> = vec![
        ("Alpha Post", "published"),
        ("Beta Post", "draft"),
        ("Gamma Post", "published"),
        ("Delta Post", "archived"),
        ("Epsilon Post", ""),
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

fn make_users_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("users");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("User".to_string())),
        plural: Some(LocalizedString::Plain("Users".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .unique(true)
            .build(),
        FieldDefinition::builder("name", FieldType::Text).build(),
    ];
    def.auth = Some(Auth {
        enabled: true,
        verify_email: true,
        ..Auth::default()
    });
    def
}

fn make_global_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("site_settings");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Site Settings".to_string())),
        plural: None,
    };
    def.fields = vec![
        FieldDefinition::builder("site_name", FieldType::Text).build(),
        FieldDefinition::builder("tagline", FieldType::Text).build(),
    ];
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

fn make_articles_with_join_tables() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.timestamps = true;
    def.fields = vec![
        make_field("title", FieldType::Text),
        FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build(),
        FieldDefinition::builder("links", FieldType::Array)
            .fields(vec![
                make_field("url", FieldType::Text),
                make_field("label", FieldType::Text),
            ])
            .build(),
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![
                BlockDefinition::new("paragraph", vec![make_field("text", FieldType::Textarea)]),
                BlockDefinition::new("image", vec![make_field("url", FieldType::Text)]),
            ])
            .build(),
    ];
    def
}

fn setup_articles() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    CollectionDefinition,
) {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_articles_with_join_tables();
    let mut tags_def = CollectionDefinition::new("tags");
    tags_def.timestamps = true;
    tags_def.fields = vec![make_field("name", FieldType::Text)];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
        reg.register_collection(tags_def);
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");
    (_tmp, pool, def)
}

#[test]
fn coerce_number_invalid_returns_null() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let mut def = CollectionDefinition::new("metrics2");
    def.timestamps = true;
    def.fields = vec![make_field("score", FieldType::Number)];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync");

    let mut data = HashMap::new();
    data.insert("score".to_string(), "abc".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "metrics2", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");
    assert!(doc.get("score").unwrap().is_null());
}

#[test]
fn coerce_number_empty_returns_null() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let mut def = CollectionDefinition::new("metrics3");
    def.timestamps = true;
    def.fields = vec![make_field("score", FieldType::Number)];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync");

    let mut data = HashMap::new();
    data.insert("score".to_string(), "".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "metrics3", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");
    assert!(doc.get("score").unwrap().is_null());
}

#[test]
fn coerce_text_empty_returns_null() {
    let (_tmp, pool, def) = seed_posts();
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Non-empty".to_string());
    data.insert("status".to_string(), "".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "posts", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");
    assert!(doc.get("status").unwrap().is_null());
}

#[test]
fn checkbox_default_when_field_missing() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let mut def = CollectionDefinition::new("checks");
    def.timestamps = true;
    def.fields = vec![
        make_field("title", FieldType::Text),
        FieldDefinition::builder("enabled", FieldType::Checkbox).build(),
    ];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync");

    // Create without providing "enabled" — should default to 0
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "checks", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");
    assert_eq!(doc.get("enabled").unwrap().as_i64(), Some(0));
}

#[test]
fn apply_select_keeps_id() {
    let mut doc = crap_cms::core::Document::new("test-id".to_string());
    doc.fields.insert("title".to_string(), json!("Test"));
    doc.fields.insert("body".to_string(), json!("Content"));
    doc.created_at = Some("2024-01-01".to_string());

    query::apply_select_to_document(&mut doc, &["title".to_string()]);
    assert_eq!(doc.id, "test-id"); // id always preserved
    assert!(doc.fields.contains_key("title"));
    assert!(!doc.fields.contains_key("body"));
}

#[test]
fn apply_select_group_prefix() {
    let mut doc = crap_cms::core::Document::new("test-id".to_string());
    doc.fields
        .insert("seo__title".to_string(), json!("SEO Title"));
    doc.fields
        .insert("seo__description".to_string(), json!("SEO Desc"));
    doc.fields.insert("other".to_string(), json!("Other"));

    query::apply_select_to_document(&mut doc, &["seo".to_string()]);
    assert!(doc.fields.contains_key("seo__title"));
    assert!(doc.fields.contains_key("seo__description"));
    assert!(!doc.fields.contains_key("other"));
}

// ── 1F. Schema Migration Edge Cases ──────────────────────────────────────────

#[test]
fn sync_creates_auth_columns() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_users_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");

    // Verify auth columns exist by using them
    let mut conn = pool.get().expect("conn");
    let mut data = HashMap::new();
    data.insert("email".to_string(), "test@example.com".to_string());
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "users", &def, &data, None).expect("Create");
    // These should not error — columns must exist
    query::update_password(&tx, "users", &doc.id, "password123").expect("update_password");
    query::set_reset_token(&tx, "users", &doc.id, "token", 9999999).expect("set_reset_token");
    query::set_verification_token(&tx, "users", &doc.id, "vtoken", 9999999999)
        .expect("set_verification_token");
    tx.commit().expect("Commit");
}

#[test]
fn sync_creates_join_tables() {
    let (_tmp, pool, def) = setup_articles();
    let conn = pool.get().expect("DB connection");

    // Verify junction tables exist by querying them
    let tags_result = query::find_related_ids(&conn, "articles", "tags", "nonexistent", None);
    assert!(tags_result.is_ok());

    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let links_result = query::find_array_rows(
        &conn,
        "articles",
        "links",
        "nonexistent",
        &links_field.fields,
        None,
    );
    assert!(links_result.is_ok());

    let blocks_result = query::find_block_rows(&conn, "articles", "content", "nonexistent", None);
    assert!(blocks_result.is_ok());
}

#[test]
fn alter_adds_new_field_column() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let mut def = make_posts_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("First sync");

    // Add a new field
    def.fields.push(make_field("excerpt", FieldType::Text));
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Second sync");

    // Verify we can use the new column
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    data.insert("excerpt".to_string(), "A short excerpt".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "posts", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");
    assert_eq!(doc.get_str("excerpt"), Some("A short excerpt"));
}

#[test]
fn alter_adds_auth_columns_on_upgrade() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();

    // Start without auth
    let mut def = CollectionDefinition::new("members");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .unique(true)
            .build(),
        make_field("name", FieldType::Text),
    ];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("First sync");

    // Upgrade to auth
    def.auth = Some(Auth {
        enabled: true,
        verify_email: true,
        ..Auth::default()
    });
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Second sync");

    // Verify auth columns work
    let mut data = HashMap::new();
    data.insert("email".to_string(), "member@test.com".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "members", &def, &data, None).expect("Create");
    query::update_password(&tx, "members", &doc.id, "pass").expect("update_password");
    query::set_verification_token(&tx, "members", &doc.id, "tok", 9999999999)
        .expect("set_verification_token");
    tx.commit().expect("Commit");
}

#[test]
fn sync_adds_locale_columns() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let mut def = CollectionDefinition::new("pages");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .localized(true)
            .build(),
        make_field("slug_field", FieldType::Text),
    ];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }

    let locale_config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };
    migrate::sync_all(&pool, &registry, &locale_config).expect("Sync");

    // Create with locale context — should write to title__en
    let locale_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let mut data = HashMap::new();
    data.insert("title".to_string(), "English Title".to_string());
    data.insert("slug_field".to_string(), "test".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).expect("Create");
    tx.commit().expect("Commit");
    assert_eq!(doc.get_str("title"), Some("English Title"));
}

#[test]
fn sync_global_creates_and_seeds() {
    let (_tmp, pool, def) = setup_global();
    let conn = pool.get().expect("DB connection");

    // Check the default row was created
    let doc = query::get_global(&conn, "site_settings", &def, None).expect("Get global failed");
    assert_eq!(doc.id, "default");
    assert!(doc.created_at.is_some());
}

// ── Group Field Tests ─────────────────────────────────────────────────────────

fn make_group_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("pages_with_seo");
    def.timestamps = true;
    def.fields = vec![
        make_field("title", FieldType::Text),
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                make_field("meta_title", FieldType::Text),
                make_field("meta_description", FieldType::Text),
            ])
            .build(),
    ];
    def
}

fn setup_group_collection() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    CollectionDefinition,
) {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_group_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync");
    (_tmp, pool, def)
}

#[test]
fn create_with_group_flattens_to_columns() {
    let (_tmp, pool, def) = setup_group_collection();
    let mut data = HashMap::new();
    data.insert("title".to_string(), "My Page".to_string());
    data.insert("seo__meta_title".to_string(), "Page Title".to_string());
    data.insert(
        "seo__meta_description".to_string(),
        "Page description".to_string(),
    );

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "pages_with_seo", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");

    // Before hydration, the fields are stored as seo__meta_title
    assert_eq!(doc.get_str("seo__meta_title"), Some("Page Title"));
    assert_eq!(
        doc.get_str("seo__meta_description"),
        Some("Page description")
    );
}

#[test]
fn hydrate_returns_grouped_fields() {
    let (_tmp, pool, def) = setup_group_collection();
    let mut data = HashMap::new();
    data.insert("title".to_string(), "My Page".to_string());
    data.insert("seo__meta_title".to_string(), "SEO Title".to_string());
    data.insert("seo__meta_description".to_string(), "SEO Desc".to_string());

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "pages_with_seo", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("conn");
    let mut doc = query::find_by_id(&conn, "pages_with_seo", &def, &doc.id, None)
        .expect("Find")
        .expect("Not found");
    query::hydrate_document(&conn, "pages_with_seo", &def.fields, &mut doc, None, None)
        .expect("Hydrate");

    // After hydration, seo should be a nested object
    let seo = doc.get("seo").expect("seo should exist");
    assert!(seo.is_object());
    assert_eq!(
        seo.get("meta_title").unwrap().as_str().unwrap(),
        "SEO Title"
    );
    assert_eq!(
        seo.get("meta_description").unwrap().as_str().unwrap(),
        "SEO Desc"
    );
}

#[test]
fn update_group_subfield() {
    let (_tmp, pool, def) = setup_group_collection();
    let mut data = HashMap::new();
    data.insert("title".to_string(), "My Page".to_string());
    data.insert("seo__meta_title".to_string(), "Original Title".to_string());
    data.insert(
        "seo__meta_description".to_string(),
        "Original Desc".to_string(),
    );

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "pages_with_seo", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");

    // Update only meta_title
    let mut update = HashMap::new();
    update.insert("seo__meta_title".to_string(), "New Title".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let updated =
        query::update(&tx, "pages_with_seo", &def, &doc.id, &update, None).expect("Update");
    tx.commit().expect("Commit");

    assert_eq!(updated.get_str("seo__meta_title"), Some("New Title"));
    // Description should be unchanged
    assert_eq!(
        updated.get_str("seo__meta_description"),
        Some("Original Desc")
    );
}

#[test]
fn select_group_prefix_in_find() {
    let (_tmp, pool, def) = setup_group_collection();
    let mut data = HashMap::new();
    data.insert("title".to_string(), "My Page".to_string());
    data.insert("seo__meta_title".to_string(), "SEO Title".to_string());
    data.insert("seo__meta_description".to_string(), "SEO Desc".to_string());

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    query::create(&tx, "pages_with_seo", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");

    // Select only "seo" — should include all seo__* sub-fields
    let mut q = query::FindQuery::new();
    q.select = Some(vec!["seo".to_string()]);
    let docs = ops::find_documents(&pool, "pages_with_seo", &def, &q, None).expect("Find");
    assert!(!docs.is_empty());
    let doc = &docs[0];
    assert!(doc.fields.contains_key("seo__meta_title"));
    assert!(doc.fields.contains_key("seo__meta_description"));
    // title should NOT be present
    assert!(!doc.fields.contains_key("title"));
}

// ── 2. count() Function Tests ─────────────────────────────────────────────────

#[test]
fn count_all_documents() {
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

    let conn = pool.get().expect("DB connection");
    let total = query::count(&conn, "posts", &def, &[], None).expect("Count failed");
    assert_eq!(total, 3);
}

#[test]
fn count_with_filter() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_posts_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");

    let statuses = ["published", "draft", "published"];
    for (i, status) in statuses.iter().enumerate() {
        let mut data = HashMap::new();
        data.insert("title".to_string(), format!("Post {}", i));
        data.insert("status".to_string(), status.to_string());
        let mut conn = pool.get().expect("DB connection");
        let tx = conn.transaction().expect("Start transaction");
        query::create(&tx, "posts", &def, &data, None).expect("Create failed");
        tx.commit().expect("Commit");
    }

    let conn = pool.get().expect("DB connection");
    let filters = vec![query::FilterClause::Single(query::Filter {
        field: "status".to_string(),
        op: query::FilterOp::Equals("published".to_string()),
    })];
    let count = query::count(&conn, "posts", &def, &filters, None).expect("Count failed");
    assert_eq!(count, 2);
}

#[test]
fn count_empty_collection() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_posts_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");

    let conn = pool.get().expect("DB connection");
    let total = query::count(&conn, "posts", &def, &[], None).expect("Count failed");
    assert_eq!(total, 0);
}

// ── 3. ops:: Pool Wrapper Tests ───────────────────────────────────────────────

#[test]
fn ops_count_documents() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_posts_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");

    for i in 0..3 {
        let mut data = HashMap::new();
        data.insert("title".to_string(), format!("Post {}", i));
        let mut conn = pool.get().expect("DB connection");
        let tx = conn.transaction().expect("Start transaction");
        query::create(&tx, "posts", &def, &data, None).expect("Create failed");
        tx.commit().expect("Commit");
    }

    let count = ops::count_documents(&pool, "posts", &def, &[], None).expect("Count failed");
    assert_eq!(count, 3);
}

#[test]
fn ops_get_global() {
    let (_tmp, pool, def) = setup_global();

    let doc = ops::get_global(&pool, "site_settings", &def, None).expect("ops::get_global failed");
    assert_eq!(doc.id, "default");
    assert!(doc.created_at.is_some());
}

// ── 4. Contains Filter LIKE Escaping (Bug Fix Tests) ──────────────────────────

#[test]
fn contains_filter_escapes_percent() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_posts_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");

    // Create two documents: one with "50% off" and one with "100 items"
    let titles = vec!["50% off", "100 items"];
    for title in &titles {
        let mut data = HashMap::new();
        data.insert("title".to_string(), title.to_string());
        let mut conn = pool.get().expect("DB connection");
        let tx = conn.transaction().expect("Start transaction");
        query::create(&tx, "posts", &def, &data, None).expect("Create failed");
        tx.commit().expect("Commit");
    }

    // Filter with Contains("50%") — should only match "50% off", NOT everything
    let mut q = query::FindQuery::new();
    q.filters = vec![query::FilterClause::Single(query::Filter {
        field: "title".to_string(),
        op: query::FilterOp::Contains("50%".to_string()),
    })];
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(
        docs.len(),
        1,
        "Contains('50%') should only match one document"
    );
    assert_eq!(docs[0].get_str("title"), Some("50% off"));
}

#[test]
fn contains_filter_escapes_underscore() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_posts_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");

    // Create two documents: "a_b" and "axb"
    let titles = vec!["a_b", "axb"];
    for title in &titles {
        let mut data = HashMap::new();
        data.insert("title".to_string(), title.to_string());
        let mut conn = pool.get().expect("DB connection");
        let tx = conn.transaction().expect("Start transaction");
        query::create(&tx, "posts", &def, &data, None).expect("Create failed");
        tx.commit().expect("Commit");
    }

    // Filter with Contains("a_b") — should only match literal "a_b", NOT "axb"
    let mut q = query::FindQuery::new();
    q.filters = vec![query::FilterClause::Single(query::Filter {
        field: "title".to_string(),
        op: query::FilterOp::Contains("a_b".to_string()),
    })];
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(
        docs.len(),
        1,
        "Contains('a_b') should only match literal underscore"
    );
    assert_eq!(docs[0].get_str("title"), Some("a_b"));
}

// ── 5. validate_query_fields Tests ────────────────────────────────────────────

#[test]
fn validate_query_fields_passes_valid() {
    let def = make_posts_def();
    let mut q = query::FindQuery::new();
    q.filters = vec![query::FilterClause::Single(query::Filter {
        field: "title".to_string(),
        op: query::FilterOp::Equals("test".to_string()),
    })];
    q.order_by = Some("status".to_string());
    let result = query::validate_query_fields(&def, &q, None);
    assert!(
        result.is_ok(),
        "Valid fields should pass validation: {:?}",
        result.err()
    );
}

#[test]
fn validate_query_fields_rejects_invalid_filter() {
    let def = make_posts_def();
    let mut q = query::FindQuery::new();
    q.filters = vec![query::FilterClause::Single(query::Filter {
        field: "nonexistent_field".to_string(),
        op: query::FilterOp::Equals("test".to_string()),
    })];
    let result = query::validate_query_fields(&def, &q, None);
    assert!(result.is_err(), "Invalid filter field should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("nonexistent_field"),
        "Error should mention the invalid field name, got: {}",
        err_msg
    );
}

#[test]
fn validate_query_fields_rejects_invalid_order() {
    let def = make_posts_def();
    let mut q = query::FindQuery::new();
    q.order_by = Some("nonexistent_field".to_string());
    let result = query::validate_query_fields(&def, &q, None);
    assert!(result.is_err(), "Invalid order_by field should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("nonexistent_field"),
        "Error should mention the invalid field name, got: {}",
        err_msg
    );
}

// ── 6. Migration DEFAULT Value Escaping (Bug Fix Test) ────────────────────────

#[test]
fn migrate_default_value_with_quotes() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let mut def = CollectionDefinition::new("books");
    def.timestamps = true;
    def.fields = vec![
        make_field("title", FieldType::Text),
        FieldDefinition::builder("publisher", FieldType::Text)
            .default_value(json!("O'Reilly"))
            .build(),
    ];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale)
        .expect("Sync should not fail on default value with quotes");

    // Create a document without providing the publisher field — should use the default
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Rust Programming".to_string());
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let doc = query::create(&tx, "books", &def, &data, None).expect("Create failed");
    tx.commit().expect("Commit");

    // The default value should be "O'Reilly" (with the apostrophe properly escaped)
    assert_eq!(doc.get_str("publisher"), Some("O'Reilly"));
}

// ── 7. coerce_value Edge Cases (Tested Indirectly) ────────────────────────────

#[test]
fn create_checkbox_truthy_values() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let mut def = CollectionDefinition::new("flags");
    def.timestamps = true;
    def.fields = vec![
        make_field("label", FieldType::Text),
        FieldDefinition::builder("active", FieldType::Checkbox).build(),
    ];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync");

    // All truthy values should store as 1
    for truthy in &["on", "true", "1", "yes"] {
        let mut data = HashMap::new();
        data.insert("label".to_string(), format!("truthy_{}", truthy));
        data.insert("active".to_string(), truthy.to_string());
        let mut conn = pool.get().expect("conn");
        let tx = conn.transaction().expect("tx");
        let doc = query::create(&tx, "flags", &def, &data, None).expect("Create");
        tx.commit().expect("Commit");
        assert_eq!(
            doc.get("active").unwrap().as_i64(),
            Some(1),
            "Checkbox value '{}' should coerce to 1",
            truthy
        );
    }
}

#[test]
fn create_checkbox_falsy_values() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let mut def = CollectionDefinition::new("flags2");
    def.timestamps = true;
    def.fields = vec![
        make_field("label", FieldType::Text),
        FieldDefinition::builder("active", FieldType::Checkbox).build(),
    ];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync");

    // All falsy values should store as 0
    for falsy in &["off", "false", "0"] {
        let mut data = HashMap::new();
        data.insert("label".to_string(), format!("falsy_{}", falsy));
        data.insert("active".to_string(), falsy.to_string());
        let mut conn = pool.get().expect("conn");
        let tx = conn.transaction().expect("tx");
        let doc = query::create(&tx, "flags2", &def, &data, None).expect("Create");
        tx.commit().expect("Commit");
        assert_eq!(
            doc.get("active").unwrap().as_i64(),
            Some(0),
            "Checkbox value '{}' should coerce to 0",
            falsy
        );
    }
}

#[test]
fn create_number_invalid_stores_null() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let mut def = CollectionDefinition::new("metrics_invalid");
    def.timestamps = true;
    def.fields = vec![make_field("score", FieldType::Number)];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync");

    let mut data = HashMap::new();
    data.insert("score".to_string(), "abc".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "metrics_invalid", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");
    assert!(
        doc.get("score").unwrap().is_null(),
        "Invalid number 'abc' should store as null"
    );
}

#[test]
fn create_text_empty_stores_null() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_posts_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync");

    let mut data = HashMap::new();
    data.insert("title".to_string(), "Has empty status".to_string());
    data.insert("status".to_string(), "".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "posts", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");
    assert!(
        doc.get("status").unwrap().is_null(),
        "Empty text string should store as null"
    );
}
