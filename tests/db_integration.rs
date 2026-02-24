use std::collections::{HashMap, HashSet};

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::{
    CollectionAccess, CollectionAdmin, CollectionAuth, CollectionDefinition, CollectionHooks,
    CollectionLabels, GlobalDefinition,
};
use crap_cms::core::field::{
    BlockDefinition, FieldAccess, FieldAdmin, FieldDefinition, FieldHooks, FieldType,
    LocalizedString, RelationshipConfig,
};
use crap_cms::core::Registry;
use crap_cms::db::{migrate, ops, pool, query};

fn make_posts_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "posts".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Post".to_string())),
            plural: Some(LocalizedString::Plain("Posts".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "title".to_string(),
                field_type: FieldType::Text,
                required: true,
                unique: false,
                validate: None,
                default_value: None,
                options: Vec::new(),
                admin: FieldAdmin::default(),
                hooks: FieldHooks::default(),
                access: FieldAccess::default(),
                relationship: None,
                fields: Vec::new(),
                blocks: Vec::new(),
                localized: false,
            },
            FieldDefinition {
                name: "status".to_string(),
                field_type: FieldType::Select,
                required: false,
                unique: false,
                validate: None,
                default_value: Some(serde_json::json!("draft")),
                options: Vec::new(),
                admin: FieldAdmin::default(),
                hooks: FieldHooks::default(),
                access: FieldAccess::default(),
                relationship: None,
                fields: Vec::new(),
                blocks: Vec::new(),
                localized: false,
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    }
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
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Failed to sync schema");

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

    let deleted = ops::find_document_by_id(&pool, "posts", &def, &doc_id, None)
        .expect("Query failed");
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
    def.fields.push(FieldDefinition {
        name: "body".to_string(),
        field_type: FieldType::Textarea,
        required: false,
        unique: false,
        validate: None,
        default_value: None,
        options: Vec::new(),
        admin: FieldAdmin::default(),
        hooks: FieldHooks::default(),
        access: FieldAccess::default(),
        relationship: None,
        fields: Vec::new(),
        blocks: Vec::new(),
        localized: false,
    });
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
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Second sync with timestamps");

    // Verify we can query (the bug: SELECT ... created_at, updated_at would fail)
    let find_query = query::FindQuery {
        filters: vec![],
        order_by: None,
        limit: None,
        offset: None,
        select: None,
    };
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

    let find_query = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "nonexistent".to_string(),
            op: query::FilterOp::Equals("test".to_string()),
        })],
        ..Default::default()
    };

    let result = ops::find_documents(&pool, "posts", &def, &find_query, None);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Invalid field 'nonexistent'"));
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

    let find_query = query::FindQuery {
        order_by: Some("nonexistent".to_string()),
        ..Default::default()
    };

    let result = ops::find_documents(&pool, "posts", &def, &find_query, None);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Invalid field 'nonexistent'"));
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

    let find_query = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "1=1; DROP TABLE posts; --".to_string(),
            op: query::FilterOp::Equals("x".to_string()),
        })],
        ..Default::default()
    };

    let result = ops::find_documents(&pool, "posts", &def, &find_query, None);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("Invalid field"), "Expected invalid field error, got: {}", err_msg);
}

// ── Filter operator tests ────────────────────────────────────────────────────

/// Set up a fresh DB with 5 seeded posts for filter testing.
/// Returns (pool, def, _tmp). Hold _tmp to keep the temp dir alive.
fn seed_posts() -> (tempfile::TempDir, crap_cms::db::DbPool, CollectionDefinition) {
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

#[test]
fn filter_equals() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::Equals("published".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(docs.len(), 2);
    for doc in &docs {
        assert_eq!(doc.get_str("status"), Some("published"));
    }
}

#[test]
fn filter_not_equals() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::NotEquals("published".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    // != excludes "published" (2 rows), but NULL != 'published' is NULL (falsy in SQL)
    // so only "draft" and "archived" match
    assert_eq!(docs.len(), 2);
    let statuses: Vec<_> = docs.iter().filter_map(|d| d.get_str("status")).collect();
    assert!(statuses.contains(&"draft"));
    assert!(statuses.contains(&"archived"));
}

#[test]
fn filter_contains() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::Contains("eta".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("title"), Some("Beta Post"));
}

#[test]
fn filter_like() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::Like("A%".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("title"), Some("Alpha Post"));
}

#[test]
fn filter_greater_than() {
    let (_tmp, pool, def) = seed_posts();
    // SQLite text comparison: "D" < "Delta Post", "E" < "Epsilon Post", "G" < "Gamma Post"
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::GreaterThan("D".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    let titles: Vec<_> = docs.iter().filter_map(|d| d.get_str("title")).collect();
    // "Delta Post", "Epsilon Post", "Gamma Post" are all > "D"
    assert_eq!(titles.len(), 3);
    assert!(titles.contains(&"Delta Post"));
    assert!(titles.contains(&"Epsilon Post"));
    assert!(titles.contains(&"Gamma Post"));
}

#[test]
fn filter_less_than() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::LessThan("C".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    let titles: Vec<_> = docs.iter().filter_map(|d| d.get_str("title")).collect();
    // "Alpha Post" and "Beta Post" are < "C"
    assert_eq!(titles.len(), 2);
    assert!(titles.contains(&"Alpha Post"));
    assert!(titles.contains(&"Beta Post"));
}

#[test]
fn filter_greater_than_or_equal() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::GreaterThanOrEqual("Gamma Post".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    let titles: Vec<_> = docs.iter().filter_map(|d| d.get_str("title")).collect();
    // "Gamma Post" (=) is included
    assert!(titles.contains(&"Gamma Post"));
    assert!(!titles.contains(&"Alpha Post"));
    assert!(!titles.contains(&"Beta Post"));
    assert!(!titles.contains(&"Delta Post"));
}

#[test]
fn filter_less_than_or_equal() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::LessThanOrEqual("Beta Post".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    let titles: Vec<_> = docs.iter().filter_map(|d| d.get_str("title")).collect();
    // "Alpha Post" (<) and "Beta Post" (=)
    assert_eq!(titles.len(), 2);
    assert!(titles.contains(&"Alpha Post"));
    assert!(titles.contains(&"Beta Post"));
}

#[test]
fn filter_in() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::In(vec!["draft".to_string(), "archived".to_string()]),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(docs.len(), 2);
    let statuses: Vec<_> = docs.iter().filter_map(|d| d.get_str("status")).collect();
    assert!(statuses.contains(&"draft"));
    assert!(statuses.contains(&"archived"));
}

#[test]
fn filter_not_in() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::NotIn(vec!["draft".to_string(), "archived".to_string()]),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    // NOT IN excludes draft + archived, but NULL NOT IN (...) is NULL (falsy)
    // so only the 2 published rows match
    assert_eq!(docs.len(), 2);
    for doc in &docs {
        assert_eq!(doc.get_str("status"), Some("published"));
    }
}

#[test]
fn filter_exists() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::Exists,
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    // 4 rows have status set, 1 is NULL
    assert_eq!(docs.len(), 4);
}

#[test]
fn filter_not_exists() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::NotExists,
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    // Only "Epsilon Post" has NULL status
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("title"), Some("Epsilon Post"));
}

#[test]
fn filter_or_clause() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Or(vec![
            vec![query::Filter {
                field: "title".to_string(),
                op: query::FilterOp::Contains("Alpha".to_string()),
            }],
            vec![query::Filter {
                field: "title".to_string(),
                op: query::FilterOp::Contains("Gamma".to_string()),
            }],
        ])],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(docs.len(), 2);
    let titles: Vec<_> = docs.iter().filter_map(|d| d.get_str("title")).collect();
    assert!(titles.contains(&"Alpha Post"));
    assert!(titles.contains(&"Gamma Post"));
}

#[test]
fn filter_or_multi_condition_groups() {
    let (_tmp, pool, def) = seed_posts();
    // (status = "published" AND title contains "Alpha") OR (title contains "Gamma")
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Or(vec![
            vec![
                query::Filter {
                    field: "status".to_string(),
                    op: query::FilterOp::Equals("published".to_string()),
                },
                query::Filter {
                    field: "title".to_string(),
                    op: query::FilterOp::Contains("Alpha".to_string()),
                },
            ],
            vec![query::Filter {
                field: "title".to_string(),
                op: query::FilterOp::Contains("Gamma".to_string()),
            }],
        ])],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(docs.len(), 2);
    let titles: Vec<_> = docs.iter().filter_map(|d| d.get_str("title")).collect();
    assert!(titles.contains(&"Alpha Post"));
    assert!(titles.contains(&"Gamma Post"));
}

#[test]
fn filter_or_with_and_top_level() {
    let (_tmp, pool, def) = seed_posts();
    // status = "published" AND (title contains "Alpha" OR title contains "Beta")
    let q = query::FindQuery {
        filters: vec![
            query::FilterClause::Single(query::Filter {
                field: "status".to_string(),
                op: query::FilterOp::Equals("published".to_string()),
            }),
            query::FilterClause::Or(vec![
                vec![query::Filter {
                    field: "title".to_string(),
                    op: query::FilterOp::Contains("Alpha".to_string()),
                }],
                vec![query::Filter {
                    field: "title".to_string(),
                    op: query::FilterOp::Contains("Beta".to_string()),
                }],
            ]),
        ],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    // Alpha is published, Beta is draft → only Alpha matches
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("title"), Some("Alpha Post"));
}

#[test]
fn select_fields_in_find() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery {
        select: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert!(!docs.is_empty());
    for doc in &docs {
        // id is always included
        assert!(!doc.id.is_empty());
        // title should be present
        assert!(doc.fields.contains_key("title"), "title should be in fields");
        // status should NOT be present (not selected)
        assert!(!doc.fields.contains_key("status"), "status should not be in fields");
    }
}

#[test]
fn select_fields_apply_to_document() {
    let (_tmp, pool, def) = seed_posts();
    let mut docs = ops::find_documents(&pool, "posts", &def, &query::FindQuery::default(), None)
        .expect("Find failed");
    assert!(!docs.is_empty());
    let doc = &mut docs[0];
    // Before stripping, both fields exist
    assert!(doc.fields.contains_key("title"));
    assert!(doc.fields.contains_key("status"));
    query::apply_select_to_document(doc, &["title".to_string()]);
    // After stripping, only title remains
    assert!(doc.fields.contains_key("title"));
    assert!(!doc.fields.contains_key("status"));
}

#[test]
fn filter_combined_and() {
    let (_tmp, pool, def) = seed_posts();
    // status = "published" AND title contains "Alpha"
    let q = query::FindQuery {
        filters: vec![
            query::FilterClause::Single(query::Filter {
                field: "status".to_string(),
                op: query::FilterOp::Equals("published".to_string()),
            }),
            query::FilterClause::Single(query::Filter {
                field: "title".to_string(),
                op: query::FilterOp::Contains("Alpha".to_string()),
            }),
        ],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("title"), Some("Alpha Post"));
    assert_eq!(docs[0].get_str("status"), Some("published"));
}

#[test]
fn count_with_filters() {
    let (_tmp, pool, def) = seed_posts();
    let filters = vec![query::FilterClause::Single(query::Filter {
        field: "status".to_string(),
        op: query::FilterOp::Equals("published".to_string()),
    })];
    let count = ops::count_documents(&pool, "posts", &def, &filters, None).expect("Count failed");
    assert_eq!(count, 2);
}

// ── Helper: auth collection definition ────────────────────────────────────────

fn make_users_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "users".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("User".to_string())),
            plural: Some(LocalizedString::Plain("Users".to_string())),
        },
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "email".to_string(),
                field_type: FieldType::Email,
                required: true,
                unique: true,
                validate: None,
                default_value: None,
                options: Vec::new(),
                admin: FieldAdmin::default(),
                hooks: FieldHooks::default(),
                access: FieldAccess::default(),
                relationship: None,
                fields: Vec::new(),
                blocks: Vec::new(),
                localized: false,
            },
            FieldDefinition {
                name: "name".to_string(),
                field_type: FieldType::Text,
                required: false,
                unique: false,
                validate: None,
                default_value: None,
                options: Vec::new(),
                admin: FieldAdmin::default(),
                hooks: FieldHooks::default(),
                access: FieldAccess::default(),
                relationship: None,
                fields: Vec::new(),
                blocks: Vec::new(),
                localized: false,
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: Some(CollectionAuth {
            enabled: true,
            verify_email: true,
            ..CollectionAuth::default()
        }),
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    }
}

fn setup_auth_collection() -> (tempfile::TempDir, crap_cms::db::DbPool, CollectionDefinition) {
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
    let result = query::find_by_email(&conn, "users", &def, "alice@example.com")
        .expect("Query failed");
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
        .expect("Query failed").expect("User not found");

    // Initially no password hash
    let hash = query::get_password_hash(&conn, "users", &user.id)
        .expect("Get hash failed");
    assert!(hash.is_none());

    // Update password
    query::update_password(&conn, "users", &user.id, "secret123")
        .expect("Update password failed");

    // Verify hash is now set
    let hash = query::get_password_hash(&conn, "users", &user.id)
        .expect("Get hash failed");
    assert!(hash.is_some());
    let hash_str = hash.unwrap();
    assert!(hash_str.starts_with("$argon2"));
}

#[test]
fn get_password_hash_missing_user() {
    let (_tmp, pool, _def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");
    let result = query::get_password_hash(&conn, "users", "nonexistent-id")
        .expect("Query failed");
    assert!(result.is_none());
}

#[test]
fn set_and_find_reset_token() {
    let (_tmp, pool, def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");

    let user = query::find_by_email(&conn, "users", &def, "alice@example.com")
        .expect("Query failed").expect("User not found");

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
    let result = query::find_by_reset_token(&conn, "users", &def, "wrong-token")
        .expect("Query failed");
    assert!(result.is_none());
}

#[test]
fn clear_reset_token() {
    let (_tmp, pool, def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");

    let user = query::find_by_email(&conn, "users", &def, "alice@example.com")
        .expect("Query failed").expect("User not found");

    let exp = chrono::Utc::now().timestamp() + 3600;
    query::set_reset_token(&conn, "users", &user.id, "token-to-clear", exp)
        .expect("Set failed");

    query::clear_reset_token(&conn, "users", &user.id)
        .expect("Clear failed");

    let found = query::find_by_reset_token(&conn, "users", &def, "token-to-clear")
        .expect("Query failed");
    assert!(found.is_none());
}

#[test]
fn set_and_find_verification_token() {
    let (_tmp, pool, def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");

    let user = query::find_by_email(&conn, "users", &def, "alice@example.com")
        .expect("Query failed").expect("User not found");

    query::set_verification_token(&conn, "users", &user.id, "verify-abc")
        .expect("Set verification token failed");

    let found = query::find_by_verification_token(&conn, "users", &def, "verify-abc")
        .expect("Find failed");
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, user.id);
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
        .expect("Query failed").expect("User not found");

    // Initially not verified
    let verified = query::is_verified(&conn, "users", &user.id)
        .expect("Check failed");
    assert!(!verified);

    // Mark verified
    query::mark_verified(&conn, "users", &user.id)
        .expect("Mark verified failed");

    // Now verified
    let verified = query::is_verified(&conn, "users", &user.id)
        .expect("Check failed");
    assert!(verified);
}

#[test]
fn is_verified_default_false() {
    let (_tmp, pool, def) = setup_auth_collection();
    let conn = pool.get().expect("DB connection");

    let user = query::find_by_email(&conn, "users", &def, "alice@example.com")
        .expect("Query failed").expect("User not found");

    let verified = query::is_verified(&conn, "users", &user.id)
        .expect("Check failed");
    assert!(!verified);
}

#[test]
fn count_where_field_eq_basic() {
    let (_tmp, pool, _def) = seed_posts();
    let conn = pool.get().expect("DB connection");
    let count = query::count_where_field_eq(&conn, "posts", "status", "published", None)
        .expect("Count failed");
    assert_eq!(count, 2);
}

#[test]
fn count_where_field_eq_with_exclude() {
    let (_tmp, pool, def) = seed_posts();
    let conn = pool.get().expect("DB connection");

    // Find one published doc to exclude
    let docs = ops::find_documents(
        &pool, "posts", &def,
        &query::FindQuery {
            filters: vec![query::FilterClause::Single(query::Filter {
                field: "status".to_string(),
                op: query::FilterOp::Equals("published".to_string()),
            })],
            ..Default::default()
        },
        None,
    ).expect("Find failed");
    assert!(!docs.is_empty());
    let exclude_id = &docs[0].id;

    let count = query::count_where_field_eq(&conn, "posts", "status", "published", Some(exclude_id))
        .expect("Count failed");
    assert_eq!(count, 1);
}

// ── 1B. Globals ───────────────────────────────────────────────────────────────

fn make_global_def() -> GlobalDefinition {
    GlobalDefinition {
        slug: "site_settings".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Site Settings".to_string())),
            plural: None,
        },
        fields: vec![
            FieldDefinition {
                name: "site_name".to_string(),
                field_type: FieldType::Text,
                required: false,
                unique: false,
                validate: None,
                default_value: None,
                options: Vec::new(),
                admin: FieldAdmin::default(),
                hooks: FieldHooks::default(),
                access: FieldAccess::default(),
                relationship: None,
                fields: Vec::new(),
                blocks: Vec::new(),
                localized: false,
            },
            FieldDefinition {
                name: "tagline".to_string(),
                field_type: FieldType::Text,
                required: false,
                unique: false,
                validate: None,
                default_value: None,
                options: Vec::new(),
                admin: FieldAdmin::default(),
                hooks: FieldHooks::default(),
                access: FieldAccess::default(),
                relationship: None,
                fields: Vec::new(),
                blocks: Vec::new(),
                localized: false,
            },
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        live: None,
    }
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
    let doc = query::get_global(&conn, "site_settings", &def, None)
        .expect("Get global failed");
    assert_eq!(doc.id, "default");
}

#[test]
fn get_global_returns_default() {
    let (_tmp, pool, def) = setup_global();
    let conn = pool.get().expect("DB connection");
    let doc = query::get_global(&conn, "site_settings", &def, None)
        .expect("Get global failed");
    assert_eq!(doc.id, "default");
    // Fields should be null/empty initially
    assert!(doc.get_str("site_name").is_none() || doc.get("site_name") == Some(&serde_json::Value::Null));
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
    let doc2 = query::get_global(&conn, "site_settings", &def, None)
        .expect("Get global failed");
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
        query::update_global(&tx, "site_settings", &def, &data, None)
            .expect("Update failed");
        tx.commit().expect("Commit");
    }

    // Second update: only set site_name
    {
        let mut conn = pool.get().expect("DB connection");
        let tx = conn.transaction().expect("Start transaction");
        let mut data = HashMap::new();
        data.insert("site_name".to_string(), "New Name".to_string());
        query::update_global(&tx, "site_settings", &def, &data, None)
            .expect("Update failed");
        tx.commit().expect("Commit");
    }

    // Tagline should still be the original
    let conn = pool.get().expect("DB connection");
    let doc = query::get_global(&conn, "site_settings", &def, None)
        .expect("Get global failed");
    assert_eq!(doc.get_str("site_name"), Some("New Name"));
    assert_eq!(doc.get_str("tagline"), Some("Original Tagline"));
}

// ── 1C. Join Tables ──────────────────────────────────────────────────────────

fn make_field(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition {
        name: name.to_string(),
        field_type,
        required: false,
        unique: false,
        validate: None,
        default_value: None,
        options: Vec::new(),
        admin: FieldAdmin::default(),
        hooks: FieldHooks::default(),
        access: FieldAccess::default(),
        relationship: None,
        fields: Vec::new(),
        blocks: Vec::new(),
        localized: false,
    }
}

fn make_articles_with_join_tables() -> CollectionDefinition {
    CollectionDefinition {
        slug: "articles".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("title", FieldType::Text),
            // has-many relationship
            FieldDefinition {
                name: "tags".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig {
                    collection: "tags".to_string(),
                    has_many: true,
                    max_depth: None,
                }),
                ..make_field("tags", FieldType::Relationship)
            },
            // array field with sub-fields
            FieldDefinition {
                name: "links".to_string(),
                field_type: FieldType::Array,
                fields: vec![
                    make_field("url", FieldType::Text),
                    make_field("label", FieldType::Text),
                ],
                ..make_field("links", FieldType::Array)
            },
            // blocks field
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Blocks,
                blocks: vec![
                    BlockDefinition {
                        block_type: "paragraph".to_string(),
                        fields: vec![make_field("text", FieldType::Textarea)],
                        label: None,
                    },
                    BlockDefinition {
                        block_type: "image".to_string(),
                        fields: vec![make_field("url", FieldType::Text)],
                        label: None,
                    },
                ],
                ..make_field("content", FieldType::Blocks)
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    }
}

fn setup_articles() -> (tempfile::TempDir, crap_cms::db::DbPool, CollectionDefinition) {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_articles_with_join_tables();
    let tags_def = CollectionDefinition {
        slug: "tags".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![make_field("name", FieldType::Text)],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    };
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
        reg.register_collection(tags_def);
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");
    (_tmp, pool, def)
}

#[test]
fn set_and_find_related_ids() {
    let (_tmp, pool, def) = setup_articles();

    // Create an article
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test Article".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    let ids = vec!["tag-1".to_string(), "tag-2".to_string(), "tag-3".to_string()];
    query::set_related_ids(&tx, "articles", "tags", &doc.id, &ids)
        .expect("Set related ids failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_related_ids(&conn, "articles", "tags", &doc.id)
        .expect("Find related ids failed");
    assert_eq!(found, ids);
}

#[test]
fn set_related_ids_replaces_existing() {
    let (_tmp, pool, def) = setup_articles();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    // First set
    query::set_related_ids(&tx, "articles", "tags", &doc.id, &["a".to_string(), "b".to_string()])
        .expect("Set failed");

    // Replace
    query::set_related_ids(&tx, "articles", "tags", &doc.id, &["c".to_string(), "d".to_string()])
        .expect("Set failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_related_ids(&conn, "articles", "tags", &doc.id)
        .expect("Find failed");
    assert_eq!(found, vec!["c".to_string(), "d".to_string()]);
}

#[test]
fn find_related_ids_empty() {
    let (_tmp, pool, def) = setup_articles();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_related_ids(&conn, "articles", "tags", &doc.id)
        .expect("Find failed");
    assert!(found.is_empty());
}

#[test]
fn set_and_find_array_rows() {
    let (_tmp, pool, def) = setup_articles();
    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let sub_fields = &links_field.fields;

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    let rows = vec![
        {
            let mut m = HashMap::new();
            m.insert("url".to_string(), "https://example.com".to_string());
            m.insert("label".to_string(), "Example".to_string());
            m
        },
        {
            let mut m = HashMap::new();
            m.insert("url".to_string(), "https://rust-lang.org".to_string());
            m.insert("label".to_string(), "Rust".to_string());
            m
        },
    ];
    query::set_array_rows(&tx, "articles", "links", &doc.id, &rows, sub_fields)
        .expect("Set array rows failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_array_rows(&conn, "articles", "links", &doc.id, sub_fields)
        .expect("Find array rows failed");
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].get("url").unwrap().as_str().unwrap(), "https://example.com");
    assert_eq!(found[0].get("label").unwrap().as_str().unwrap(), "Example");
    assert_eq!(found[1].get("url").unwrap().as_str().unwrap(), "https://rust-lang.org");
    // Each row should have an id
    assert!(found[0].get("id").is_some());
}

#[test]
fn set_array_rows_replaces_existing() {
    let (_tmp, pool, def) = setup_articles();
    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let sub_fields = &links_field.fields;

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    let rows1 = vec![{
        let mut m = HashMap::new();
        m.insert("url".to_string(), "https://old.com".to_string());
        m.insert("label".to_string(), "Old".to_string());
        m
    }];
    query::set_array_rows(&tx, "articles", "links", &doc.id, &rows1, sub_fields)
        .expect("Set failed");

    let rows2 = vec![{
        let mut m = HashMap::new();
        m.insert("url".to_string(), "https://new.com".to_string());
        m.insert("label".to_string(), "New".to_string());
        m
    }];
    query::set_array_rows(&tx, "articles", "links", &doc.id, &rows2, sub_fields)
        .expect("Set failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_array_rows(&conn, "articles", "links", &doc.id, sub_fields)
        .expect("Find failed");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].get("url").unwrap().as_str().unwrap(), "https://new.com");
}

#[test]
fn find_array_rows_empty() {
    let (_tmp, pool, def) = setup_articles();
    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let sub_fields = &links_field.fields;

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_array_rows(&conn, "articles", "links", &doc.id, sub_fields)
        .expect("Find failed");
    assert!(found.is_empty());
}

#[test]
fn set_and_find_block_rows() {
    let (_tmp, pool, def) = setup_articles();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    let blocks = vec![
        serde_json::json!({"_block_type": "paragraph", "text": "Hello world"}),
        serde_json::json!({"_block_type": "image", "url": "/img/test.png"}),
    ];
    query::set_block_rows(&tx, "articles", "content", &doc.id, &blocks)
        .expect("Set block rows failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_block_rows(&conn, "articles", "content", &doc.id)
        .expect("Find block rows failed");
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].get("_block_type").unwrap().as_str().unwrap(), "paragraph");
    assert_eq!(found[0].get("text").unwrap().as_str().unwrap(), "Hello world");
    assert_eq!(found[1].get("_block_type").unwrap().as_str().unwrap(), "image");
    assert_eq!(found[1].get("url").unwrap().as_str().unwrap(), "/img/test.png");
    // Each block should have an id
    assert!(found[0].get("id").is_some());
}

#[test]
fn set_block_rows_replaces_existing() {
    let (_tmp, pool, def) = setup_articles();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    let blocks1 = vec![serde_json::json!({"_block_type": "paragraph", "text": "Old"})];
    query::set_block_rows(&tx, "articles", "content", &doc.id, &blocks1)
        .expect("Set failed");

    let blocks2 = vec![serde_json::json!({"_block_type": "image", "url": "/new.png"})];
    query::set_block_rows(&tx, "articles", "content", &doc.id, &blocks2)
        .expect("Set failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_block_rows(&conn, "articles", "content", &doc.id)
        .expect("Find failed");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].get("_block_type").unwrap().as_str().unwrap(), "image");
}

#[test]
fn find_block_rows_empty() {
    let (_tmp, pool, def) = setup_articles();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_block_rows(&conn, "articles", "content", &doc.id)
        .expect("Find failed");
    assert!(found.is_empty());
}

#[test]
fn hydrate_document_populates_join_data() {
    let (_tmp, pool, def) = setup_articles();
    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    // Set up join table data
    query::set_related_ids(&tx, "articles", "tags", &doc.id, &["t1".to_string(), "t2".to_string()])
        .expect("Set related failed");
    let rows = vec![{
        let mut m = HashMap::new();
        m.insert("url".to_string(), "https://example.com".to_string());
        m.insert("label".to_string(), "Ex".to_string());
        m
    }];
    query::set_array_rows(&tx, "articles", "links", &doc.id, &rows, &links_field.fields)
        .expect("Set array failed");
    let blocks = vec![serde_json::json!({"_block_type": "paragraph", "text": "Hi"})];
    query::set_block_rows(&tx, "articles", "content", &doc.id, &blocks)
        .expect("Set blocks failed");
    tx.commit().expect("Commit");

    // Hydrate
    let conn = pool.get().expect("DB connection");
    let mut doc = query::find_by_id(&conn, "articles", &def, &doc.id, None)
        .expect("Find failed").expect("Not found");
    query::hydrate_document(&conn, "articles", &def, &mut doc, None)
        .expect("Hydrate failed");

    // Verify tags (has-many relationship)
    let tags = doc.get("tags").expect("tags should exist");
    assert!(tags.is_array());
    let tags_arr = tags.as_array().unwrap();
    assert_eq!(tags_arr.len(), 2);
    assert_eq!(tags_arr[0].as_str().unwrap(), "t1");

    // Verify links (array)
    let links = doc.get("links").expect("links should exist");
    assert!(links.is_array());
    let links_arr = links.as_array().unwrap();
    assert_eq!(links_arr.len(), 1);
    assert_eq!(links_arr[0].get("url").unwrap().as_str().unwrap(), "https://example.com");

    // Verify content (blocks)
    let content = doc.get("content").expect("content should exist");
    assert!(content.is_array());
    let blocks_arr = content.as_array().unwrap();
    assert_eq!(blocks_arr.len(), 1);
    assert_eq!(blocks_arr[0].get("_block_type").unwrap().as_str().unwrap(), "paragraph");
}

#[test]
fn save_join_table_data_from_hashmap() {
    let (_tmp, pool, def) = setup_articles();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    // Prepare join table data as JSON values
    let mut jt_data: HashMap<String, serde_json::Value> = HashMap::new();
    jt_data.insert("tags".to_string(), serde_json::json!(["tag-a", "tag-b"]));
    jt_data.insert("links".to_string(), serde_json::json!([
        {"url": "https://a.com", "label": "A"},
        {"url": "https://b.com", "label": "B"},
    ]));
    jt_data.insert("content".to_string(), serde_json::json!([
        {"_block_type": "paragraph", "text": "Content block"},
    ]));

    query::save_join_table_data(&tx, "articles", &def, &doc.id, &jt_data)
        .expect("Save join table data failed");
    tx.commit().expect("Commit");

    // Verify
    let conn = pool.get().expect("DB connection");
    let tags = query::find_related_ids(&conn, "articles", "tags", &doc.id)
        .expect("Find tags failed");
    assert_eq!(tags, vec!["tag-a", "tag-b"]);

    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let links = query::find_array_rows(&conn, "articles", "links", &doc.id, &links_field.fields)
        .expect("Find links failed");
    assert_eq!(links.len(), 2);

    let blocks = query::find_block_rows(&conn, "articles", "content", &doc.id)
        .expect("Find blocks failed");
    assert_eq!(blocks.len(), 1);
}

#[test]
fn save_join_table_data_partial_update() {
    let (_tmp, pool, def) = setup_articles();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    // First: set tags and links
    let mut jt_data: HashMap<String, serde_json::Value> = HashMap::new();
    jt_data.insert("tags".to_string(), serde_json::json!(["tag-1", "tag-2"]));
    jt_data.insert("links".to_string(), serde_json::json!([{"url": "https://a.com", "label": "A"}]));
    query::save_join_table_data(&tx, "articles", &def, &doc.id, &jt_data)
        .expect("Save failed");

    // Second: only update tags (links should be unchanged)
    let mut jt_data2: HashMap<String, serde_json::Value> = HashMap::new();
    jt_data2.insert("tags".to_string(), serde_json::json!(["tag-3"]));
    query::save_join_table_data(&tx, "articles", &def, &doc.id, &jt_data2)
        .expect("Save failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let tags = query::find_related_ids(&conn, "articles", "tags", &doc.id)
        .expect("Find tags failed");
    assert_eq!(tags, vec!["tag-3"]);

    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let links = query::find_array_rows(&conn, "articles", "links", &doc.id, &links_field.fields)
        .expect("Find links failed");
    // Links should be unchanged (not in the second update)
    assert_eq!(links.len(), 1);
}

// ── 1D. Relationship Population / Depth ───────────────────────────────────────

fn make_categories_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "categories".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("name", FieldType::Text),
            // Self-referencing parent (for circular ref test)
            FieldDefinition {
                name: "parent".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig {
                    collection: "categories".to_string(),
                    has_many: false,
                    max_depth: None,
                }),
                ..make_field("parent", FieldType::Relationship)
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    }
}

fn make_posts_with_category() -> CollectionDefinition {
    CollectionDefinition {
        slug: "posts_v2".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("title", FieldType::Text),
            // has-one relationship to categories
            FieldDefinition {
                name: "category".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig {
                    collection: "categories".to_string(),
                    has_many: false,
                    max_depth: None,
                }),
                ..make_field("category", FieldType::Relationship)
            },
            // has-many relationship to categories
            FieldDefinition {
                name: "secondary_categories".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig {
                    collection: "categories".to_string(),
                    has_many: true,
                    max_depth: None,
                }),
                ..make_field("secondary_categories", FieldType::Relationship)
            },
            // field with max_depth cap
            FieldDefinition {
                name: "limited_cat".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig {
                    collection: "categories".to_string(),
                    has_many: false,
                    max_depth: Some(0),
                }),
                ..make_field("limited_cat", FieldType::Relationship)
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    }
}

fn setup_posts_categories() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    crap_cms::core::SharedRegistry,
    CollectionDefinition,
    CollectionDefinition,
) {
    let (_tmp, pool) = create_test_pool();
    let shared_registry = Registry::shared();
    let cats_def = make_categories_def();
    let posts_def = make_posts_with_category();
    {
        let mut reg = shared_registry.write().unwrap();
        reg.register_collection(cats_def.clone());
        reg.register_collection(posts_def.clone());
    }
    migrate::sync_all(&pool, &shared_registry, &CrapConfig::default().locale).expect("Sync failed");

    (_tmp, pool, shared_registry, posts_def, cats_def)
}

#[test]
fn populate_depth_0_leaves_ids() {
    let (_tmp, pool, registry, posts_def, cats_def) = setup_posts_categories();

    // Create a category
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut cat_data = HashMap::new();
    cat_data.insert("name".to_string(), "Tech".to_string());
    let cat = query::create(&tx, "categories", &cats_def, &cat_data, None).expect("Create cat failed");

    let mut post_data = HashMap::new();
    post_data.insert("title".to_string(), "My Post".to_string());
    post_data.insert("category".to_string(), cat.id.clone());
    let mut post = query::create(&tx, "posts_v2", &posts_def, &post_data, None).expect("Create post failed");
    tx.commit().expect("Commit");

    // Populate at depth 0 — should be a no-op
    let conn = pool.get().expect("DB connection");
    let mut visited = HashSet::new();
    query::populate_relationships(&conn, &registry.read().unwrap(), "posts_v2", &posts_def, &mut post, 0, &mut visited, None)
        .expect("Populate failed");

    // category should still be an ID string
    assert_eq!(post.get_str("category"), Some(cat.id.as_str()));
}

#[test]
fn populate_depth_1_hydrates_has_one() {
    let (_tmp, pool, registry, posts_def, cats_def) = setup_posts_categories();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut cat_data = HashMap::new();
    cat_data.insert("name".to_string(), "Tech".to_string());
    let cat = query::create(&tx, "categories", &cats_def, &cat_data, None).expect("Create cat");

    let mut post_data = HashMap::new();
    post_data.insert("title".to_string(), "My Post".to_string());
    post_data.insert("category".to_string(), cat.id.clone());
    let mut post = query::create(&tx, "posts_v2", &posts_def, &post_data, None).expect("Create post");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let mut visited = HashSet::new();
    query::populate_relationships(&conn, &registry.read().unwrap(), "posts_v2", &posts_def, &mut post, 1, &mut visited, None)
        .expect("Populate failed");

    // category should be a full document object
    let cat_val = post.get("category").expect("category should exist");
    assert!(cat_val.is_object(), "category should be an object, got: {:?}", cat_val);
    assert_eq!(cat_val.get("name").unwrap().as_str().unwrap(), "Tech");
    assert_eq!(cat_val.get("id").unwrap().as_str().unwrap(), cat.id);
}

#[test]
fn populate_depth_1_hydrates_has_many() {
    let (_tmp, pool, registry, posts_def, cats_def) = setup_posts_categories();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    let mut cat1_data = HashMap::new();
    cat1_data.insert("name".to_string(), "Tech".to_string());
    let cat1 = query::create(&tx, "categories", &cats_def, &cat1_data, None).expect("Create cat1");

    let mut cat2_data = HashMap::new();
    cat2_data.insert("name".to_string(), "Science".to_string());
    let cat2 = query::create(&tx, "categories", &cats_def, &cat2_data, None).expect("Create cat2");

    let mut post_data = HashMap::new();
    post_data.insert("title".to_string(), "Multi-cat Post".to_string());
    let mut post = query::create(&tx, "posts_v2", &posts_def, &post_data, None).expect("Create post");

    query::set_related_ids(&tx, "posts_v2", "secondary_categories", &post.id, &[cat1.id.clone(), cat2.id.clone()])
        .expect("Set related failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    query::hydrate_document(&conn, "posts_v2", &posts_def, &mut post, None)
        .expect("Hydrate failed");

    let mut visited = HashSet::new();
    query::populate_relationships(&conn, &registry.read().unwrap(), "posts_v2", &posts_def, &mut post, 1, &mut visited, None)
        .expect("Populate failed");

    let sec_cats = post.get("secondary_categories").expect("should exist");
    assert!(sec_cats.is_array());
    let arr = sec_cats.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    // Should be full objects, not IDs
    assert!(arr[0].is_object());
    assert!(arr[0].get("name").is_some());
}

#[test]
fn populate_circular_ref_stops() {
    let (_tmp, pool, registry, _posts_def, cats_def) = setup_posts_categories();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    // Create cat A → parent B → parent A (circular)
    let mut a_data = HashMap::new();
    a_data.insert("name".to_string(), "A".to_string());
    let cat_a = query::create(&tx, "categories", &cats_def, &a_data, None).expect("Create A");

    let mut b_data = HashMap::new();
    b_data.insert("name".to_string(), "B".to_string());
    b_data.insert("parent".to_string(), cat_a.id.clone());
    let cat_b = query::create(&tx, "categories", &cats_def, &b_data, None).expect("Create B");

    // Update A to point to B
    let mut update = HashMap::new();
    update.insert("parent".to_string(), cat_b.id.clone());
    let mut cat_a = query::update(&tx, "categories", &cats_def, &cat_a.id, &update, None).expect("Update A");
    tx.commit().expect("Commit");

    // Populate at depth 10 — should not infinite loop
    let conn = pool.get().expect("DB connection");
    let mut visited = HashSet::new();
    query::populate_relationships(&conn, &registry.read().unwrap(), "categories", &cats_def, &mut cat_a, 10, &mut visited, None)
        .expect("Populate should not loop");
    // Should complete without panic
}

#[test]
fn populate_missing_related_doc() {
    let (_tmp, pool, registry, posts_def, _cats_def) = setup_posts_categories();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut post_data = HashMap::new();
    post_data.insert("title".to_string(), "Orphaned".to_string());
    post_data.insert("category".to_string(), "nonexistent-cat-id".to_string());
    let mut post = query::create(&tx, "posts_v2", &posts_def, &post_data, None).expect("Create post");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let mut visited = HashSet::new();
    query::populate_relationships(&conn, &registry.read().unwrap(), "posts_v2", &posts_def, &mut post, 1, &mut visited, None)
        .expect("Populate should handle missing");

    // Category should remain as a string ID (not populated)
    assert_eq!(post.get_str("category"), Some("nonexistent-cat-id"));
}

#[test]
fn populate_respects_field_max_depth() {
    let (_tmp, pool, registry, posts_def, cats_def) = setup_posts_categories();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    let mut cat_data = HashMap::new();
    cat_data.insert("name".to_string(), "Tech".to_string());
    let cat = query::create(&tx, "categories", &cats_def, &cat_data, None).expect("Create cat");

    let mut post_data = HashMap::new();
    post_data.insert("title".to_string(), "Post".to_string());
    post_data.insert("limited_cat".to_string(), cat.id.clone());
    let mut post = query::create(&tx, "posts_v2", &posts_def, &post_data, None).expect("Create post");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let mut visited = HashSet::new();
    // Even with depth=5, the limited_cat field has max_depth=0, so it shouldn't populate
    query::populate_relationships(&conn, &registry.read().unwrap(), "posts_v2", &posts_def, &mut post, 5, &mut visited, None)
        .expect("Populate failed");

    // limited_cat should remain as string ID (max_depth=0 prevents population)
    assert_eq!(post.get_str("limited_cat"), Some(cat.id.as_str()));
}

// ── 1E. Type Coercion & Edge Cases ────────────────────────────────────────────

#[test]
fn coerce_checkbox_values() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = CollectionDefinition {
        slug: "forms".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "active".to_string(),
                field_type: FieldType::Checkbox,
                ..make_field("active", FieldType::Checkbox)
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    };
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
    let def = CollectionDefinition {
        slug: "metrics".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![make_field("score", FieldType::Number)],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    };
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

#[test]
fn coerce_number_invalid_returns_null() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = CollectionDefinition {
        slug: "metrics2".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![make_field("score", FieldType::Number)],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    };
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
    let def = CollectionDefinition {
        slug: "metrics3".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![make_field("score", FieldType::Number)],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    };
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
    let def = CollectionDefinition {
        slug: "checks".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("title", FieldType::Text),
            FieldDefinition {
                name: "enabled".to_string(),
                field_type: FieldType::Checkbox,
                ..make_field("enabled", FieldType::Checkbox)
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    };
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
    doc.fields.insert("title".to_string(), serde_json::json!("Test"));
    doc.fields.insert("body".to_string(), serde_json::json!("Content"));
    doc.created_at = Some("2024-01-01".to_string());

    query::apply_select_to_document(&mut doc, &["title".to_string()]);
    assert_eq!(doc.id, "test-id"); // id always preserved
    assert!(doc.fields.contains_key("title"));
    assert!(!doc.fields.contains_key("body"));
}

#[test]
fn apply_select_group_prefix() {
    let mut doc = crap_cms::core::Document::new("test-id".to_string());
    doc.fields.insert("seo__title".to_string(), serde_json::json!("SEO Title"));
    doc.fields.insert("seo__description".to_string(), serde_json::json!("SEO Desc"));
    doc.fields.insert("other".to_string(), serde_json::json!("Other"));

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
    query::set_verification_token(&tx, "users", &doc.id, "vtoken").expect("set_verification_token");
    tx.commit().expect("Commit");
}

#[test]
fn sync_creates_join_tables() {
    let (_tmp, pool, def) = setup_articles();
    let conn = pool.get().expect("DB connection");

    // Verify junction tables exist by querying them
    let tags_result = query::find_related_ids(&conn, "articles", "tags", "nonexistent");
    assert!(tags_result.is_ok());

    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let links_result = query::find_array_rows(&conn, "articles", "links", "nonexistent", &links_field.fields);
    assert!(links_result.is_ok());

    let blocks_result = query::find_block_rows(&conn, "articles", "content", "nonexistent");
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
    let mut def = CollectionDefinition {
        slug: "members".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "email".to_string(),
                field_type: FieldType::Email,
                required: true,
                unique: true,
                ..make_field("email", FieldType::Email)
            },
            make_field("name", FieldType::Text),
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    };
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("First sync");

    // Upgrade to auth
    def.auth = Some(CollectionAuth {
        enabled: true,
        verify_email: true,
        ..CollectionAuth::default()
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
    query::set_verification_token(&tx, "members", &doc.id, "tok").expect("set_verification_token");
    tx.commit().expect("Commit");
}

#[test]
fn sync_adds_locale_columns() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = CollectionDefinition {
        slug: "pages".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "title".to_string(),
                field_type: FieldType::Text,
                localized: true,
                ..make_field("title", FieldType::Text)
            },
            make_field("slug_field", FieldType::Text),
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    };
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
    let doc = query::get_global(&conn, "site_settings", &def, None)
        .expect("Get global failed");
    assert_eq!(doc.id, "default");
    assert!(doc.created_at.is_some());
}

// ── Group Field Tests ─────────────────────────────────────────────────────────

fn make_group_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "pages_with_seo".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("title", FieldType::Text),
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    make_field("meta_title", FieldType::Text),
                    make_field("meta_description", FieldType::Text),
                ],
                ..make_field("seo", FieldType::Group)
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    }
}

fn setup_group_collection() -> (tempfile::TempDir, crap_cms::db::DbPool, CollectionDefinition) {
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
    data.insert("seo__meta_description".to_string(), "Page description".to_string());

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "pages_with_seo", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");

    // Before hydration, the fields are stored as seo__meta_title
    assert_eq!(doc.get_str("seo__meta_title"), Some("Page Title"));
    assert_eq!(doc.get_str("seo__meta_description"), Some("Page description"));
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
        .expect("Find").expect("Not found");
    query::hydrate_document(&conn, "pages_with_seo", &def, &mut doc, None)
        .expect("Hydrate");

    // After hydration, seo should be a nested object
    let seo = doc.get("seo").expect("seo should exist");
    assert!(seo.is_object());
    assert_eq!(seo.get("meta_title").unwrap().as_str().unwrap(), "SEO Title");
    assert_eq!(seo.get("meta_description").unwrap().as_str().unwrap(), "SEO Desc");
}

#[test]
fn update_group_subfield() {
    let (_tmp, pool, def) = setup_group_collection();
    let mut data = HashMap::new();
    data.insert("title".to_string(), "My Page".to_string());
    data.insert("seo__meta_title".to_string(), "Original Title".to_string());
    data.insert("seo__meta_description".to_string(), "Original Desc".to_string());

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "pages_with_seo", &def, &data, None).expect("Create");
    tx.commit().expect("Commit");

    // Update only meta_title
    let mut update = HashMap::new();
    update.insert("seo__meta_title".to_string(), "New Title".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let updated = query::update(&tx, "pages_with_seo", &def, &doc.id, &update, None).expect("Update");
    tx.commit().expect("Commit");

    assert_eq!(updated.get_str("seo__meta_title"), Some("New Title"));
    // Description should be unchanged
    assert_eq!(updated.get_str("seo__meta_description"), Some("Original Desc"));
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
    let q = query::FindQuery {
        select: Some(vec!["seo".to_string()]),
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "pages_with_seo", &def, &q, None).expect("Find");
    assert!(!docs.is_empty());
    let doc = &docs[0];
    assert!(doc.fields.contains_key("seo__meta_title"));
    assert!(doc.fields.contains_key("seo__meta_description"));
    // title should NOT be present
    assert!(!doc.fields.contains_key("title"));
}

// ── Locale-Aware Query Tests ──────────────────────────────────────────────────

fn make_localized_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "localized_pages".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            FieldDefinition {
                name: "title".to_string(),
                field_type: FieldType::Text,
                localized: true,
                ..make_field("title", FieldType::Text)
            },
            make_field("slug_field", FieldType::Text),
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    }
}

fn setup_localized() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    CollectionDefinition,
    LocaleConfig,
) {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_localized_def();
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
    (_tmp, pool, def, locale_config)
}

#[test]
fn create_with_locale_writes_correct_column() {
    let (_tmp, pool, def, locale_config) = setup_localized();

    let locale_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let mut data = HashMap::new();
    data.insert("title".to_string(), "English Title".to_string());
    data.insert("slug_field".to_string(), "test-page".to_string());

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "localized_pages", &def, &data, Some(&locale_ctx)).expect("Create");
    tx.commit().expect("Commit");

    // In Single mode, the title is returned as "title" (aliased)
    assert_eq!(doc.get_str("title"), Some("English Title"));
}

#[test]
fn find_with_locale_coalesce_fallback() {
    let (_tmp, pool, def, locale_config) = setup_localized();

    // Create with English locale
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let mut data = HashMap::new();
    data.insert("title".to_string(), "English Title".to_string());
    data.insert("slug_field".to_string(), "test-page".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    query::create(&tx, "localized_pages", &def, &data, Some(&en_ctx)).expect("Create");
    tx.commit().expect("Commit");

    // Read with German locale — should fall back to English
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };
    let docs = ops::find_documents(
        &pool, "localized_pages", &def,
        &query::FindQuery::default(),
        Some(&de_ctx),
    ).expect("Find");
    assert!(!docs.is_empty());
    // With fallback enabled, should get the English value
    assert_eq!(docs[0].get_str("title"), Some("English Title"));
}

#[test]
fn find_all_locales_returns_nested() {
    let (_tmp, pool, def, locale_config) = setup_localized();

    // Create with English
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let mut data = HashMap::new();
    data.insert("title".to_string(), "English Title".to_string());
    data.insert("slug_field".to_string(), "page".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "localized_pages", &def, &data, Some(&en_ctx)).expect("Create");
    tx.commit().expect("Commit");

    // Update with German
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };
    let mut de_data = HashMap::new();
    de_data.insert("title".to_string(), "Deutscher Titel".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    query::update(&tx, "localized_pages", &def, &doc.id, &de_data, Some(&de_ctx)).expect("Update");
    tx.commit().expect("Commit");

    // Find with All mode — should return nested locale object
    let all_ctx = query::LocaleContext {
        mode: query::LocaleMode::All,
        config: locale_config.clone(),
    };
    let docs = ops::find_documents(
        &pool, "localized_pages", &def,
        &query::FindQuery::default(),
        Some(&all_ctx),
    ).expect("Find");
    assert!(!docs.is_empty());

    let title_val = docs[0].get("title").expect("title should exist");
    assert!(title_val.is_object(), "title should be a locale object, got: {:?}", title_val);
    assert_eq!(title_val.get("en").unwrap().as_str().unwrap(), "English Title");
    assert_eq!(title_val.get("de").unwrap().as_str().unwrap(), "Deutscher Titel");
}

#[test]
fn update_with_locale() {
    let (_tmp, pool, def, locale_config) = setup_localized();

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Original".to_string());
    data.insert("slug_field".to_string(), "test".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "localized_pages", &def, &data, Some(&en_ctx)).expect("Create");
    tx.commit().expect("Commit");

    // Update the English title
    let mut update = HashMap::new();
    update.insert("title".to_string(), "Updated English".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let updated = query::update(&tx, "localized_pages", &def, &doc.id, &update, Some(&en_ctx)).expect("Update");
    tx.commit().expect("Commit");
    assert_eq!(updated.get_str("title"), Some("Updated English"));
}

#[test]
fn filter_on_localized_field() {
    let (_tmp, pool, def, locale_config) = setup_localized();

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };

    // Create two documents
    for (title, slug) in &[("Hello World", "hello"), ("Goodbye World", "goodbye")] {
        let mut data = HashMap::new();
        data.insert("title".to_string(), title.to_string());
        data.insert("slug_field".to_string(), slug.to_string());
        let mut conn = pool.get().expect("conn");
        let tx = conn.transaction().expect("tx");
        query::create(&tx, "localized_pages", &def, &data, Some(&en_ctx)).expect("Create");
        tx.commit().expect("Commit");
    }

    // Filter on localized field
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::Contains("Hello".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "localized_pages", &def, &q, Some(&en_ctx))
        .expect("Find");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("title"), Some("Hello World"));
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

    let doc = ops::get_global(&pool, "site_settings", &def, None)
        .expect("ops::get_global failed");
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
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::Contains("50%".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(docs.len(), 1, "Contains('50%') should only match one document");
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
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::Contains("a_b".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(docs.len(), 1, "Contains('a_b') should only match literal underscore");
    assert_eq!(docs[0].get_str("title"), Some("a_b"));
}

// ── 5. validate_query_fields Tests ────────────────────────────────────────────

#[test]
fn validate_query_fields_passes_valid() {
    let def = make_posts_def();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::Equals("test".to_string()),
        })],
        order_by: Some("status".to_string()),
        ..Default::default()
    };
    let result = query::validate_query_fields(&def, &q, None);
    assert!(result.is_ok(), "Valid fields should pass validation: {:?}", result.err());
}

#[test]
fn validate_query_fields_rejects_invalid_filter() {
    let def = make_posts_def();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "nonexistent_field".to_string(),
            op: query::FilterOp::Equals("test".to_string()),
        })],
        ..Default::default()
    };
    let result = query::validate_query_fields(&def, &q, None);
    assert!(result.is_err(), "Invalid filter field should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("nonexistent_field"), "Error should mention the invalid field name, got: {}", err_msg);
}

#[test]
fn validate_query_fields_rejects_invalid_order() {
    let def = make_posts_def();
    let q = query::FindQuery {
        order_by: Some("nonexistent_field".to_string()),
        ..Default::default()
    };
    let result = query::validate_query_fields(&def, &q, None);
    assert!(result.is_err(), "Invalid order_by field should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("nonexistent_field"), "Error should mention the invalid field name, got: {}", err_msg);
}

// ── 6. Migration DEFAULT Value Escaping (Bug Fix Test) ────────────────────────

#[test]
fn migrate_default_value_with_quotes() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = CollectionDefinition {
        slug: "books".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("title", FieldType::Text),
            FieldDefinition {
                name: "publisher".to_string(),
                field_type: FieldType::Text,
                default_value: Some(serde_json::json!("O'Reilly")),
                ..make_field("publisher", FieldType::Text)
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    };
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
    let def = CollectionDefinition {
        slug: "flags".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("label", FieldType::Text),
            FieldDefinition {
                name: "active".to_string(),
                field_type: FieldType::Checkbox,
                ..make_field("active", FieldType::Checkbox)
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    };
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
            doc.get("active").unwrap().as_i64(), Some(1),
            "Checkbox value '{}' should coerce to 1", truthy
        );
    }
}

#[test]
fn create_checkbox_falsy_values() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = CollectionDefinition {
        slug: "flags2".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("label", FieldType::Text),
            FieldDefinition {
                name: "active".to_string(),
                field_type: FieldType::Checkbox,
                ..make_field("active", FieldType::Checkbox)
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    };
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
            doc.get("active").unwrap().as_i64(), Some(0),
            "Checkbox value '{}' should coerce to 0", falsy
        );
    }
}

#[test]
fn create_number_invalid_stores_null() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = CollectionDefinition {
        slug: "metrics_invalid".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![make_field("score", FieldType::Number)],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        live: None,
            versions: None,
    };
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
    assert!(doc.get("score").unwrap().is_null(), "Invalid number 'abc' should store as null");
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
    assert!(doc.get("status").unwrap().is_null(), "Empty text string should store as null");
}
