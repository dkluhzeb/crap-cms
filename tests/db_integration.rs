use std::collections::{HashMap, HashSet};

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::{
    CollectionAccess, CollectionAdmin, CollectionAuth, CollectionDefinition, CollectionHooks,
    CollectionLabels, GlobalDefinition,
};
use crap_cms::core::field::{
    BlockDefinition, FieldDefinition, FieldType,
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
                required: true,
                ..Default::default()
            },
            FieldDefinition {
                name: "status".to_string(),
                field_type: FieldType::Select,
                default_value: Some(serde_json::json!("draft")),
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
        ..Default::default()
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
        after_cursor: None,
        before_cursor: None,
        search: None,
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
                ..Default::default()
            },
            FieldDefinition {
                name: "name".to_string(),
                ..Default::default()
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
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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

    query::set_verification_token(&conn, "users", &user.id, "verify-abc", 9999999999)
        .expect("Set verification token failed");

    let found = query::find_by_verification_token(&conn, "users", &def, "verify-abc")
        .expect("Find failed");
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
                ..Default::default()
            },
            FieldDefinition {
                name: "tagline".to_string(),
                ..Default::default()
            },
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
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
        ..Default::default()
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
                    polymorphic: vec![],
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
                        ..Default::default()
                    },
                    BlockDefinition {
                        block_type: "image".to_string(),
                        fields: vec![make_field("url", FieldType::Text)],
                        ..Default::default()
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
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
    query::set_related_ids(&tx, "articles", "tags", &doc.id, &ids, None)
        .expect("Set related ids failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_related_ids(&conn, "articles", "tags", &doc.id, None)
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
    query::set_related_ids(&tx, "articles", "tags", &doc.id, &["a".to_string(), "b".to_string()], None)
        .expect("Set failed");

    // Replace
    query::set_related_ids(&tx, "articles", "tags", &doc.id, &["c".to_string(), "d".to_string()], None)
        .expect("Set failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_related_ids(&conn, "articles", "tags", &doc.id, None)
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
    let found = query::find_related_ids(&conn, "articles", "tags", &doc.id, None)
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
    query::set_array_rows(&tx, "articles", "links", &doc.id, &rows, sub_fields, None)
        .expect("Set array rows failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_array_rows(&conn, "articles", "links", &doc.id, sub_fields, None)
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
    query::set_array_rows(&tx, "articles", "links", &doc.id, &rows1, sub_fields, None)
        .expect("Set failed");

    let rows2 = vec![{
        let mut m = HashMap::new();
        m.insert("url".to_string(), "https://new.com".to_string());
        m.insert("label".to_string(), "New".to_string());
        m
    }];
    query::set_array_rows(&tx, "articles", "links", &doc.id, &rows2, sub_fields, None)
        .expect("Set failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_array_rows(&conn, "articles", "links", &doc.id, sub_fields, None)
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
    let found = query::find_array_rows(&conn, "articles", "links", &doc.id, sub_fields, None)
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
    query::set_block_rows(&tx, "articles", "content", &doc.id, &blocks, None)
        .expect("Set block rows failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_block_rows(&conn, "articles", "content", &doc.id, None)
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
    query::set_block_rows(&tx, "articles", "content", &doc.id, &blocks1, None)
        .expect("Set failed");

    let blocks2 = vec![serde_json::json!({"_block_type": "image", "url": "/new.png"})];
    query::set_block_rows(&tx, "articles", "content", &doc.id, &blocks2, None)
        .expect("Set failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_block_rows(&conn, "articles", "content", &doc.id, None)
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
    let found = query::find_block_rows(&conn, "articles", "content", &doc.id, None)
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
    query::set_related_ids(&tx, "articles", "tags", &doc.id, &["t1".to_string(), "t2".to_string()], None)
        .expect("Set related failed");
    let rows = vec![{
        let mut m = HashMap::new();
        m.insert("url".to_string(), "https://example.com".to_string());
        m.insert("label".to_string(), "Ex".to_string());
        m
    }];
    query::set_array_rows(&tx, "articles", "links", &doc.id, &rows, &links_field.fields, None)
        .expect("Set array failed");
    let blocks = vec![serde_json::json!({"_block_type": "paragraph", "text": "Hi"})];
    query::set_block_rows(&tx, "articles", "content", &doc.id, &blocks, None)
        .expect("Set blocks failed");
    tx.commit().expect("Commit");

    // Hydrate
    let conn = pool.get().expect("DB connection");
    let mut doc = query::find_by_id(&conn, "articles", &def, &doc.id, None)
        .expect("Find failed").expect("Not found");
    query::hydrate_document(&conn, "articles", &def.fields, &mut doc, None, None)
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

    query::save_join_table_data(&tx, "articles", &def.fields, &doc.id, &jt_data, None)
        .expect("Save join table data failed");
    tx.commit().expect("Commit");

    // Verify
    let conn = pool.get().expect("DB connection");
    let tags = query::find_related_ids(&conn, "articles", "tags", &doc.id, None)
        .expect("Find tags failed");
    assert_eq!(tags, vec!["tag-a", "tag-b"]);

    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let links = query::find_array_rows(&conn, "articles", "links", &doc.id, &links_field.fields, None)
        .expect("Find links failed");
    assert_eq!(links.len(), 2);

    let blocks = query::find_block_rows(&conn, "articles", "content", &doc.id, None)
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
    query::save_join_table_data(&tx, "articles", &def.fields, &doc.id, &jt_data, None)
        .expect("Save failed");

    // Second: only update tags (links should be unchanged)
    let mut jt_data2: HashMap<String, serde_json::Value> = HashMap::new();
    jt_data2.insert("tags".to_string(), serde_json::json!(["tag-3"]));
    query::save_join_table_data(&tx, "articles", &def.fields, &doc.id, &jt_data2, None)
        .expect("Save failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let tags = query::find_related_ids(&conn, "articles", "tags", &doc.id, None)
        .expect("Find tags failed");
    assert_eq!(tags, vec!["tag-3"]);

    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let links = query::find_array_rows(&conn, "articles", "links", &doc.id, &links_field.fields, None)
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
                    polymorphic: vec![],
                }),
                ..make_field("parent", FieldType::Relationship)
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
                    polymorphic: vec![],
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
                    polymorphic: vec![],
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
                    polymorphic: vec![],
                }),
                ..make_field("limited_cat", FieldType::Relationship)
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
    query::populate_relationships(&conn, &registry.read().unwrap(), "posts_v2", &posts_def, &mut post, 0, &mut visited, None, None)
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
    query::populate_relationships(&conn, &registry.read().unwrap(), "posts_v2", &posts_def, &mut post, 1, &mut visited, None, None)
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

    query::set_related_ids(&tx, "posts_v2", "secondary_categories", &post.id, &[cat1.id.clone(), cat2.id.clone()], None)
        .expect("Set related failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    query::hydrate_document(&conn, "posts_v2", &posts_def.fields, &mut post, None, None)
        .expect("Hydrate failed");

    let mut visited = HashSet::new();
    query::populate_relationships(&conn, &registry.read().unwrap(), "posts_v2", &posts_def, &mut post, 1, &mut visited, None, None)
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
    query::populate_relationships(&conn, &registry.read().unwrap(), "categories", &cats_def, &mut cat_a, 10, &mut visited, None, None)
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
    query::populate_relationships(&conn, &registry.read().unwrap(), "posts_v2", &posts_def, &mut post, 1, &mut visited, None, None)
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
    query::populate_relationships(&conn, &registry.read().unwrap(), "posts_v2", &posts_def, &mut post, 5, &mut visited, None, None)
        .expect("Populate failed");

    // limited_cat should remain as string ID (max_depth=0 prevents population)
    assert_eq!(post.get_str("limited_cat"), Some(cat.id.as_str()));
}

// Regression: populate_relationships with localized fields on the related collection
// used to fail because find_by_ids was called without locale_ctx, generating
// `SELECT caption` instead of `SELECT caption__en` for localized columns.
#[test]
fn populate_with_localized_related_collection() {
    let (_tmp, pool) = create_test_pool();
    let shared_registry = Registry::shared();

    // "media" collection with a localized field
    let media_def = CollectionDefinition {
        slug: "media".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("url", FieldType::Text),
            FieldDefinition {
                name: "caption".to_string(),
                localized: true,
                ..make_field("caption", FieldType::Text)
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    };

    // "articles" collection with a relationship to media
    let articles_def = CollectionDefinition {
        slug: "articles".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("title", FieldType::Text),
            FieldDefinition {
                name: "image".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig {
                    collection: "media".to_string(),
                    has_many: false,
                    max_depth: None,
                    polymorphic: vec![],
                }),
                ..make_field("image", FieldType::Relationship)
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    };

    {
        let mut reg = shared_registry.write().unwrap();
        reg.register_collection(media_def.clone());
        reg.register_collection(articles_def.clone());
    }

    let locale_config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };
    migrate::sync_all(&pool, &shared_registry, &locale_config).expect("Sync failed");

    let locale_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };

    // Create a media document with localized caption
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut media_data = HashMap::new();
    media_data.insert("url".to_string(), "/img/test.png".to_string());
    media_data.insert("caption".to_string(), "Test image".to_string());
    let media_doc = query::create(&tx, "media", &media_def, &media_data, Some(&locale_ctx))
        .expect("Create media");

    // Create an article referencing the media
    let mut article_data = HashMap::new();
    article_data.insert("title".to_string(), "My Article".to_string());
    article_data.insert("image".to_string(), media_doc.id.clone());
    let mut article = query::create(&tx, "articles", &articles_def, &article_data, None)
        .expect("Create article");
    tx.commit().expect("Commit");

    // Populate at depth 1 WITH locale_ctx — this used to fail with
    // "Failed to prepare find_by_ids query on 'media'" because the populate
    // code didn't forward locale_ctx to find_by_ids.
    let conn = pool.get().expect("conn");
    let mut visited = HashSet::new();
    query::populate_relationships(
        &conn, &shared_registry.read().unwrap(), "articles", &articles_def,
        &mut article, 1, &mut visited, None, Some(&locale_ctx),
    ).expect("Populate with localized related collection should succeed");

    // image should be populated as a full object
    let img = article.get("image").expect("image field should exist");
    assert!(img.is_object(), "image should be populated object, got: {:?}", img);
    assert_eq!(img.get("url").unwrap().as_str().unwrap(), "/img/test.png");
    assert_eq!(img.get("caption").unwrap().as_str().unwrap(), "Test image");
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
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
    query::set_verification_token(&tx, "users", &doc.id, "vtoken", 9999999999).expect("set_verification_token");
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
    let links_result = query::find_array_rows(&conn, "articles", "links", "nonexistent", &links_field.fields, None);
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
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
    query::set_verification_token(&tx, "members", &doc.id, "tok", 9999999999).expect("set_verification_token");
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
                localized: true,
                ..Default::default()
            },
            make_field("slug_field", FieldType::Text),
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
    query::hydrate_document(&conn, "pages_with_seo", &def.fields, &mut doc, None, None)
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
                localized: true,
                ..Default::default()
            },
            make_field("slug_field", FieldType::Text),
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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
        mcp: Default::default(),
        live: None,
            versions: None,
            indexes: Vec::new(),
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

// ── Locale-Aware Join Table Tests ────────────────────────────────────────────

/// Collection definition with localized join-table fields (has-many, array, blocks).
fn make_localized_join_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "l10n_articles".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("slug_field", FieldType::Text),
            // Localized has-many relationship
            FieldDefinition {
                name: "tags".to_string(),
                field_type: FieldType::Relationship,
                localized: true,
                relationship: Some(RelationshipConfig {
                    collection: "tags".to_string(),
                    has_many: true,
                    max_depth: None,
                    polymorphic: vec![],
                }),
                ..Default::default()
            },
            // Localized array
            FieldDefinition {
                name: "links".to_string(),
                field_type: FieldType::Array,
                localized: true,
                fields: vec![
                    make_field("url", FieldType::Text),
                    make_field("label", FieldType::Text),
                ],
                ..Default::default()
            },
            // Localized blocks
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Blocks,
                localized: true,
                blocks: vec![
                    BlockDefinition {
                        block_type: "paragraph".to_string(),
                        fields: vec![make_field("text", FieldType::Textarea)],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            // Non-localized blocks (control: should be unaffected by locale)
            FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Blocks,
                blocks: vec![
                    BlockDefinition {
                        block_type: "kv".to_string(),
                        fields: vec![make_field("key", FieldType::Text)],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    }
}

fn setup_localized_joins() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    CollectionDefinition,
    LocaleConfig,
) {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_localized_join_def();
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
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    };
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
        reg.register_collection(tags_def);
    }
    let locale_config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };
    migrate::sync_all(&pool, &registry, &locale_config).expect("Sync");
    (_tmp, pool, def, locale_config)
}

// ── Low-level: set/find with locale scoping ──────────────────────────────────

#[test]
fn localized_related_ids_scoped_by_locale() {
    let (_tmp, pool, _def, _lc) = setup_localized_joins();
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    // Create a parent document
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &_def, &data, None).expect("Create");

    // Write English tags
    query::set_related_ids(&tx, "l10n_articles", "tags", &doc.id, &["en-tag-1".into(), "en-tag-2".into()], Some("en"))
        .expect("Set EN tags");
    // Write German tags
    query::set_related_ids(&tx, "l10n_articles", "tags", &doc.id, &["de-tag-1".into()], Some("de"))
        .expect("Set DE tags");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("conn");
    // Read English tags — should only get EN
    let en_tags = query::find_related_ids(&conn, "l10n_articles", "tags", &doc.id, Some("en"))
        .expect("Find EN tags");
    assert_eq!(en_tags, vec!["en-tag-1", "en-tag-2"]);

    // Read German tags — should only get DE
    let de_tags = query::find_related_ids(&conn, "l10n_articles", "tags", &doc.id, Some("de"))
        .expect("Find DE tags");
    assert_eq!(de_tags, vec!["de-tag-1"]);
}

#[test]
fn localized_related_ids_update_one_locale_preserves_other() {
    let (_tmp, pool, _def, _lc) = setup_localized_joins();
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &_def, &data, None).expect("Create");

    // Write both locales
    query::set_related_ids(&tx, "l10n_articles", "tags", &doc.id, &["en-1".into(), "en-2".into()], Some("en"))
        .expect("Set EN");
    query::set_related_ids(&tx, "l10n_articles", "tags", &doc.id, &["de-1".into()], Some("de"))
        .expect("Set DE");

    // Overwrite EN tags — DE should be preserved
    query::set_related_ids(&tx, "l10n_articles", "tags", &doc.id, &["en-3".into()], Some("en"))
        .expect("Overwrite EN");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("conn");
    let en = query::find_related_ids(&conn, "l10n_articles", "tags", &doc.id, Some("en")).unwrap();
    assert_eq!(en, vec!["en-3"], "EN tags should be replaced");

    let de = query::find_related_ids(&conn, "l10n_articles", "tags", &doc.id, Some("de")).unwrap();
    assert_eq!(de, vec!["de-1"], "DE tags should be preserved");
}

#[test]
fn localized_array_rows_scoped_by_locale() {
    let (_tmp, pool, def, _lc) = setup_localized_joins();
    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    let en_rows = vec![HashMap::from([
        ("url".to_string(), "https://en.example.com".to_string()),
        ("label".to_string(), "English Link".to_string()),
    ])];
    let de_rows = vec![HashMap::from([
        ("url".to_string(), "https://de.example.com".to_string()),
        ("label".to_string(), "German Link".to_string()),
    ])];

    query::set_array_rows(&tx, "l10n_articles", "links", &doc.id, &en_rows, &links_field.fields, Some("en"))
        .expect("Set EN links");
    query::set_array_rows(&tx, "l10n_articles", "links", &doc.id, &de_rows, &links_field.fields, Some("de"))
        .expect("Set DE links");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("conn");
    let en = query::find_array_rows(&conn, "l10n_articles", "links", &doc.id, &links_field.fields, Some("en")).unwrap();
    assert_eq!(en.len(), 1);
    assert_eq!(en[0]["label"], "English Link");

    let de = query::find_array_rows(&conn, "l10n_articles", "links", &doc.id, &links_field.fields, Some("de")).unwrap();
    assert_eq!(de.len(), 1);
    assert_eq!(de[0]["label"], "German Link");
}

#[test]
fn localized_array_rows_update_preserves_other_locale() {
    let (_tmp, pool, def, _lc) = setup_localized_joins();
    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    let en_rows = vec![HashMap::from([
        ("url".to_string(), "https://en.example.com".to_string()),
        ("label".to_string(), "English".to_string()),
    ])];
    let de_rows = vec![HashMap::from([
        ("url".to_string(), "https://de.example.com".to_string()),
        ("label".to_string(), "Deutsch".to_string()),
    ])];

    query::set_array_rows(&tx, "l10n_articles", "links", &doc.id, &en_rows, &links_field.fields, Some("en")).unwrap();
    query::set_array_rows(&tx, "l10n_articles", "links", &doc.id, &de_rows, &links_field.fields, Some("de")).unwrap();

    // Replace EN rows — DE should remain
    let en_new = vec![HashMap::from([
        ("url".to_string(), "https://new-en.example.com".to_string()),
        ("label".to_string(), "New English".to_string()),
    ])];
    query::set_array_rows(&tx, "l10n_articles", "links", &doc.id, &en_new, &links_field.fields, Some("en")).unwrap();
    tx.commit().expect("Commit");

    let conn = pool.get().expect("conn");
    let en = query::find_array_rows(&conn, "l10n_articles", "links", &doc.id, &links_field.fields, Some("en")).unwrap();
    assert_eq!(en.len(), 1);
    assert_eq!(en[0]["label"], "New English");

    let de = query::find_array_rows(&conn, "l10n_articles", "links", &doc.id, &links_field.fields, Some("de")).unwrap();
    assert_eq!(de.len(), 1);
    assert_eq!(de[0]["label"], "Deutsch", "DE array rows should be preserved");
}

#[test]
fn localized_block_rows_scoped_by_locale() {
    let (_tmp, pool, def, _lc) = setup_localized_joins();
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    let en_blocks = vec![serde_json::json!({"_block_type": "paragraph", "text": "Hello world"})];
    let de_blocks = vec![serde_json::json!({"_block_type": "paragraph", "text": "Hallo Welt"})];

    query::set_block_rows(&tx, "l10n_articles", "content", &doc.id, &en_blocks, Some("en")).unwrap();
    query::set_block_rows(&tx, "l10n_articles", "content", &doc.id, &de_blocks, Some("de")).unwrap();
    tx.commit().expect("Commit");

    let conn = pool.get().expect("conn");
    let en = query::find_block_rows(&conn, "l10n_articles", "content", &doc.id, Some("en")).unwrap();
    assert_eq!(en.len(), 1);
    assert_eq!(en[0]["text"], "Hello world");

    let de = query::find_block_rows(&conn, "l10n_articles", "content", &doc.id, Some("de")).unwrap();
    assert_eq!(de.len(), 1);
    assert_eq!(de[0]["text"], "Hallo Welt");
}

#[test]
fn localized_block_rows_update_preserves_other_locale() {
    let (_tmp, pool, def, _lc) = setup_localized_joins();
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    query::set_block_rows(&tx, "l10n_articles", "content", &doc.id,
        &[serde_json::json!({"_block_type": "paragraph", "text": "English"})], Some("en")).unwrap();
    query::set_block_rows(&tx, "l10n_articles", "content", &doc.id,
        &[serde_json::json!({"_block_type": "paragraph", "text": "Deutsch"})], Some("de")).unwrap();

    // Replace EN blocks — DE should remain
    query::set_block_rows(&tx, "l10n_articles", "content", &doc.id,
        &[serde_json::json!({"_block_type": "paragraph", "text": "New English"})], Some("en")).unwrap();
    tx.commit().expect("Commit");

    let conn = pool.get().expect("conn");
    let en = query::find_block_rows(&conn, "l10n_articles", "content", &doc.id, Some("en")).unwrap();
    assert_eq!(en.len(), 1);
    assert_eq!(en[0]["text"], "New English");

    let de = query::find_block_rows(&conn, "l10n_articles", "content", &doc.id, Some("de")).unwrap();
    assert_eq!(de.len(), 1);
    assert_eq!(de[0]["text"], "Deutsch", "DE blocks should be preserved");
}

// ── High-level: save_join_table_data + hydrate_document with locale ──────────

#[test]
fn save_join_table_data_with_locale_scopes_writes() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Save EN join data
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert("tags".to_string(), serde_json::json!(["en-tag"]));
    en_join.insert("content".to_string(), serde_json::json!([
        {"_block_type": "paragraph", "text": "English content"}
    ]));
    en_join.insert("meta".to_string(), serde_json::json!([
        {"_block_type": "kv", "key": "shared-meta"}
    ]));
    query::save_join_table_data(&tx, "l10n_articles", &def.fields, &doc.id, &en_join, Some(&en_ctx)).unwrap();

    // Save DE join data
    let mut de_join: HashMap<String, serde_json::Value> = HashMap::new();
    de_join.insert("tags".to_string(), serde_json::json!(["de-tag"]));
    de_join.insert("content".to_string(), serde_json::json!([
        {"_block_type": "paragraph", "text": "German content"}
    ]));
    query::save_join_table_data(&tx, "l10n_articles", &def.fields, &doc.id, &de_join, Some(&de_ctx)).unwrap();
    tx.commit().expect("Commit");

    // Verify EN data
    let conn = pool.get().expect("conn");
    let en_tags = query::find_related_ids(&conn, "l10n_articles", "tags", &doc.id, Some("en")).unwrap();
    assert_eq!(en_tags, vec!["en-tag"]);
    let en_blocks = query::find_block_rows(&conn, "l10n_articles", "content", &doc.id, Some("en")).unwrap();
    assert_eq!(en_blocks.len(), 1);
    assert_eq!(en_blocks[0]["text"], "English content");

    // Verify DE data
    let de_tags = query::find_related_ids(&conn, "l10n_articles", "tags", &doc.id, Some("de")).unwrap();
    assert_eq!(de_tags, vec!["de-tag"]);
    let de_blocks = query::find_block_rows(&conn, "l10n_articles", "content", &doc.id, Some("de")).unwrap();
    assert_eq!(de_blocks.len(), 1);
    assert_eq!(de_blocks[0]["text"], "German content");

    // Non-localized "meta" field should be written without locale scoping —
    // reading with None should return it
    let meta = query::find_block_rows(&conn, "l10n_articles", "meta", &doc.id, None).unwrap();
    assert_eq!(meta.len(), 1, "Non-localized blocks should work without locale");
    assert_eq!(meta[0]["key"], "shared-meta");
}

#[test]
fn hydrate_document_with_locale_returns_correct_data() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write EN data
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert("tags".to_string(), serde_json::json!(["en-tag-1", "en-tag-2"]));
    en_join.insert("links".to_string(), serde_json::json!([
        {"url": "https://en.example.com", "label": "English"}
    ]));
    en_join.insert("content".to_string(), serde_json::json!([
        {"_block_type": "paragraph", "text": "Hello"}
    ]));
    query::save_join_table_data(&tx, "l10n_articles", &def.fields, &doc.id, &en_join, Some(&en_ctx)).unwrap();

    // Write DE data
    let mut de_join: HashMap<String, serde_json::Value> = HashMap::new();
    de_join.insert("tags".to_string(), serde_json::json!(["de-tag-1"]));
    de_join.insert("links".to_string(), serde_json::json!([
        {"url": "https://de.example.com", "label": "Deutsch"}
    ]));
    de_join.insert("content".to_string(), serde_json::json!([
        {"_block_type": "paragraph", "text": "Hallo"}
    ]));
    query::save_join_table_data(&tx, "l10n_articles", &def.fields, &doc.id, &de_join, Some(&de_ctx)).unwrap();
    tx.commit().expect("Commit");

    // Hydrate with EN locale
    let conn = pool.get().expect("conn");
    let mut en_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut en_doc, None, Some(&en_ctx)).unwrap();

    let en_tags = en_doc.get("tags").unwrap().as_array().unwrap();
    assert_eq!(en_tags.len(), 2);
    assert_eq!(en_tags[0], "en-tag-1");
    assert_eq!(en_tags[1], "en-tag-2");

    let en_links = en_doc.get("links").unwrap().as_array().unwrap();
    assert_eq!(en_links.len(), 1);
    assert_eq!(en_links[0]["label"], "English");

    let en_content = en_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(en_content.len(), 1);
    assert_eq!(en_content[0]["text"], "Hello");

    // Hydrate with DE locale
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut de_doc, None, Some(&de_ctx)).unwrap();

    let de_tags = de_doc.get("tags").unwrap().as_array().unwrap();
    assert_eq!(de_tags.len(), 1);
    assert_eq!(de_tags[0], "de-tag-1");

    let de_links = de_doc.get("links").unwrap().as_array().unwrap();
    assert_eq!(de_links.len(), 1);
    assert_eq!(de_links[0]["label"], "Deutsch");

    let de_content = de_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(de_content.len(), 1);
    assert_eq!(de_content[0]["text"], "Hallo");
}

#[test]
fn save_join_data_in_one_locale_does_not_clobber_other() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write EN content first
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert("content".to_string(), serde_json::json!([
        {"_block_type": "paragraph", "text": "English paragraph 1"},
        {"_block_type": "paragraph", "text": "English paragraph 2"},
    ]));
    query::save_join_table_data(&tx, "l10n_articles", &def.fields, &doc.id, &en_join, Some(&en_ctx)).unwrap();

    // Now write DE content — this is the bug scenario: previously this would DELETE all rows
    // (regardless of locale) and then INSERT only the DE rows, destroying EN content.
    let mut de_join: HashMap<String, serde_json::Value> = HashMap::new();
    de_join.insert("content".to_string(), serde_json::json!([
        {"_block_type": "paragraph", "text": "German paragraph"},
    ]));
    query::save_join_table_data(&tx, "l10n_articles", &def.fields, &doc.id, &de_join, Some(&de_ctx)).unwrap();
    tx.commit().expect("Commit");

    // The critical assertion: EN content must still be intact
    let conn = pool.get().expect("conn");
    let mut en_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut en_doc, None, Some(&en_ctx)).unwrap();

    let en_content = en_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(en_content.len(), 2, "English blocks must survive German write");
    assert_eq!(en_content[0]["text"], "English paragraph 1");
    assert_eq!(en_content[1]["text"], "English paragraph 2");

    // And DE content should be correct too
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut de_doc, None, Some(&de_ctx)).unwrap();

    let de_content = de_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(de_content.len(), 1);
    assert_eq!(de_content[0]["text"], "German paragraph");
}

#[test]
fn non_localized_join_field_ignores_locale_context() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write to the non-localized "meta" blocks field via save_join_table_data with a locale ctx.
    // Since meta.localized=false, the locale should be ignored (writes without _locale scoping).
    let mut join: HashMap<String, serde_json::Value> = HashMap::new();
    join.insert("meta".to_string(), serde_json::json!([
        {"_block_type": "kv", "key": "version"},
    ]));
    query::save_join_table_data(&tx, "l10n_articles", &def.fields, &doc.id, &join, Some(&en_ctx)).unwrap();
    tx.commit().expect("Commit");

    // Reading with None should find the data (no locale filter)
    let conn = pool.get().expect("conn");
    let meta = query::find_block_rows(&conn, "l10n_articles", "meta", &doc.id, None).unwrap();
    assert_eq!(meta.len(), 1);
    assert_eq!(meta[0]["key"], "version");

    // Hydrate with a different locale context — non-localized field should still appear
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };
    let mut doc2 = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut doc2, None, Some(&de_ctx)).unwrap();

    let meta2 = doc2.get("meta").unwrap().as_array().unwrap();
    assert_eq!(meta2.len(), 1, "Non-localized field should be visible regardless of locale context");
}

#[test]
fn hydrate_without_locale_returns_all_locale_rows() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write blocks in both locales
    query::set_block_rows(&tx, "l10n_articles", "content", &doc.id,
        &[serde_json::json!({"_block_type": "paragraph", "text": "EN"})], Some("en")).unwrap();
    query::set_block_rows(&tx, "l10n_articles", "content", &doc.id,
        &[serde_json::json!({"_block_type": "paragraph", "text": "DE"})], Some("de")).unwrap();
    tx.commit().expect("Commit");

    // Hydrate with None locale — should return ALL rows from all locales
    let conn = pool.get().expect("conn");
    let mut doc2 = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut doc2, None, None).unwrap();

    let content = doc2.get("content").unwrap().as_array().unwrap();
    assert_eq!(content.len(), 2, "Without locale context, all rows should be returned");

    // Hydrate with EN locale — should return only EN
    let mut en_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut en_doc, None, Some(&en_ctx)).unwrap();
    let en_content = en_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(en_content.len(), 1);
    assert_eq!(en_content[0]["text"], "EN");

    // Hydrate with DE locale — should return only DE
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut de_doc, None, Some(&de_ctx)).unwrap();
    let de_content = de_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(de_content.len(), 1);
    assert_eq!(de_content[0]["text"], "DE");
}

// ── Locale Fallback on Join Tables ───────────────────────────────────────────
// When fallback=true and requesting a non-default locale that has no data,
// join table reads should fall back to the default locale (matching the
// COALESCE behavior of regular columns).

#[test]
fn join_fallback_has_many_falls_back_to_default_locale() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    assert!(locale_config.fallback, "Precondition: fallback must be enabled");

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write EN tags only — no DE tags
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert("tags".to_string(), serde_json::json!(["tag-1", "tag-2"]));
    query::save_join_table_data(&tx, "l10n_articles", &def.fields, &doc.id, &en_join, Some(&en_ctx)).unwrap();
    tx.commit().expect("Commit");

    // Hydrate with DE locale — should fall back to EN tags
    let conn = pool.get().expect("conn");
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut de_doc, None, Some(&de_ctx)).unwrap();

    let tags = de_doc.get("tags").unwrap().as_array().unwrap();
    assert_eq!(tags.len(), 2, "DE tags should fall back to EN when empty");
    assert_eq!(tags[0], "tag-1");
    assert_eq!(tags[1], "tag-2");

    // Hydrate with EN locale — should return EN tags directly
    let mut en_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut en_doc, None, Some(&en_ctx)).unwrap();

    let en_tags = en_doc.get("tags").unwrap().as_array().unwrap();
    assert_eq!(en_tags.len(), 2);
}

#[test]
fn join_fallback_array_falls_back_to_default_locale() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write EN links only
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert("links".to_string(), serde_json::json!([
        {"url": "https://en.example.com", "label": "English Link"}
    ]));
    query::save_join_table_data(&tx, "l10n_articles", &def.fields, &doc.id, &en_join, Some(&en_ctx)).unwrap();
    tx.commit().expect("Commit");

    // Hydrate with DE locale — should fall back to EN links
    let conn = pool.get().expect("conn");
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut de_doc, None, Some(&de_ctx)).unwrap();

    let links = de_doc.get("links").unwrap().as_array().unwrap();
    assert_eq!(links.len(), 1, "DE links should fall back to EN when empty");
    assert_eq!(links[0]["label"], "English Link");
}

#[test]
fn join_fallback_blocks_falls_back_to_default_locale() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write EN blocks only
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert("content".to_string(), serde_json::json!([
        {"_block_type": "paragraph", "text": "English paragraph"}
    ]));
    query::save_join_table_data(&tx, "l10n_articles", &def.fields, &doc.id, &en_join, Some(&en_ctx)).unwrap();
    tx.commit().expect("Commit");

    // Hydrate with DE locale — should fall back to EN blocks
    let conn = pool.get().expect("conn");
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut de_doc, None, Some(&de_ctx)).unwrap();

    let content = de_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(content.len(), 1, "DE blocks should fall back to EN when empty");
    assert_eq!(content[0]["text"], "English paragraph");
}

#[test]
fn join_fallback_does_not_trigger_when_locale_has_data() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write both EN and DE content
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert("content".to_string(), serde_json::json!([
        {"_block_type": "paragraph", "text": "English"},
        {"_block_type": "paragraph", "text": "More English"},
    ]));
    query::save_join_table_data(&tx, "l10n_articles", &def.fields, &doc.id, &en_join, Some(&en_ctx)).unwrap();

    let mut de_join: HashMap<String, serde_json::Value> = HashMap::new();
    de_join.insert("content".to_string(), serde_json::json!([
        {"_block_type": "paragraph", "text": "Deutsch"},
    ]));
    query::save_join_table_data(&tx, "l10n_articles", &def.fields, &doc.id, &de_join, Some(&de_ctx)).unwrap();
    tx.commit().expect("Commit");

    // DE has its own data — should NOT fall back to EN
    let conn = pool.get().expect("conn");
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut de_doc, None, Some(&de_ctx)).unwrap();

    let content = de_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(content.len(), 1, "DE has its own data — fallback should NOT trigger");
    assert_eq!(content[0]["text"], "Deutsch");
}

#[test]
fn join_fallback_disabled_returns_empty() {
    let (_tmp, pool, def, mut locale_config) = setup_localized_joins();
    locale_config.fallback = false; // Disable fallback

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write EN content only
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert("content".to_string(), serde_json::json!([
        {"_block_type": "paragraph", "text": "English only"}
    ]));
    en_join.insert("tags".to_string(), serde_json::json!(["en-tag"]));
    query::save_join_table_data(&tx, "l10n_articles", &def.fields, &doc.id, &en_join, Some(&en_ctx)).unwrap();
    tx.commit().expect("Commit");

    // Hydrate with DE — with fallback=false, should get empty results
    let conn = pool.get().expect("conn");
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut de_doc, None, Some(&de_ctx)).unwrap();

    let content = de_doc.get("content").unwrap().as_array().unwrap();
    assert!(content.is_empty(), "With fallback=false, empty DE should NOT fall back to EN");

    let tags = de_doc.get("tags").unwrap().as_array().unwrap();
    assert!(tags.is_empty(), "With fallback=false, empty DE tags should NOT fall back to EN");
}

#[test]
fn join_fallback_default_locale_no_fallback_needed() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write EN content
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert("content".to_string(), serde_json::json!([
        {"_block_type": "paragraph", "text": "English"}
    ]));
    query::save_join_table_data(&tx, "l10n_articles", &def.fields, &doc.id, &en_join, Some(&en_ctx)).unwrap();
    tx.commit().expect("Commit");

    // Hydrate with EN (default locale) — should return EN data directly, no fallback path
    let conn = pool.get().expect("conn");
    let mut en_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap().unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut en_doc, None, Some(&en_ctx)).unwrap();

    let content = en_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["text"], "English");
}

// ── 5. Global join table support (arrays, blocks, has-many) ───────────────

fn make_global_with_join_fields() -> GlobalDefinition {
    GlobalDefinition {
        slug: "homepage".to_string(),
        labels: CollectionLabels {
            singular: Some(LocalizedString::Plain("Homepage".to_string())),
            plural: None,
        },
        fields: vec![
            FieldDefinition {
                name: "title".to_string(),
                ..Default::default()
            },
            // Group field — expanded into sub-columns (same as collections)
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    make_field("meta_title", FieldType::Text),
                    make_field("meta_description", FieldType::Textarea),
                ],
                ..Default::default()
            },
            // Array field — uses join table
            FieldDefinition {
                name: "links".to_string(),
                field_type: FieldType::Array,
                fields: vec![
                    make_field("url", FieldType::Text),
                    make_field("label", FieldType::Text),
                ],
                ..Default::default()
            },
            // Blocks field — uses join table
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Blocks,
                blocks: vec![
                    BlockDefinition {
                        block_type: "paragraph".to_string(),
                        fields: vec![make_field("text", FieldType::Textarea)],
                        ..Default::default()
                    },
                    BlockDefinition {
                        block_type: "image".to_string(),
                        fields: vec![make_field("url", FieldType::Text)],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            // Has-many relationship — uses junction table
            FieldDefinition {
                name: "featured_posts".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig {
                    collection: "posts".to_string(),
                    has_many: true,
                    max_depth: None,
                    polymorphic: vec![],
                }),
                ..Default::default()
            },
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
    }
}

fn setup_global_with_joins() -> (tempfile::TempDir, crap_cms::db::DbPool, GlobalDefinition) {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_global_with_join_fields();
    let posts_def = make_posts_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_global(def.clone());
        reg.register_collection(posts_def);
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");
    (_tmp, pool, def)
}

/// Migration creates join tables for global array/blocks/has-many fields.
#[test]
fn global_migration_creates_join_tables() {
    let (_tmp, pool, _def) = setup_global_with_joins();
    let conn = pool.get().expect("DB connection");

    // Check that join tables exist
    let check = |table: &str| -> bool {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |row| row.get(0),
        ).unwrap();
        count > 0
    };

    assert!(check("_global_homepage"), "Parent table should exist");
    assert!(check("_global_homepage_links"), "Array join table should exist");
    assert!(check("_global_homepage_content"), "Blocks join table should exist");
    assert!(check("_global_homepage_featured_posts"), "Has-many junction table should exist");
}

/// Migration does NOT create parent columns for array/blocks/has-many fields,
/// but DOES create expanded sub-columns for group fields.
#[test]
fn global_migration_parent_table_columns() {
    let (_tmp, pool, _def) = setup_global_with_joins();
    let conn = pool.get().expect("DB connection");

    let mut stmt = conn.prepare("PRAGMA table_info(_global_homepage)").unwrap();
    let columns: HashSet<String> = stmt.query_map([], |row| {
        row.get::<_, String>(1)
    }).unwrap().filter_map(|r| r.ok()).collect();

    // Should have these columns
    assert!(columns.contains("id"), "Should have id column");
    assert!(columns.contains("title"), "Should have scalar field column");
    // Group fields are expanded into sub-columns (same as collections)
    assert!(columns.contains("seo__meta_title"), "Should have group sub-field column");
    assert!(columns.contains("seo__meta_description"), "Should have group sub-field column");
    assert!(!columns.contains("seo"), "Should NOT have single group column");
    assert!(columns.contains("created_at"), "Should have created_at");
    assert!(columns.contains("updated_at"), "Should have updated_at");

    // Should NOT have these columns (they use join tables)
    assert!(!columns.contains("links"), "Array field should NOT have parent column");
    assert!(!columns.contains("content"), "Blocks field should NOT have parent column");
    assert!(!columns.contains("featured_posts"), "Has-many field should NOT have parent column");
}

/// Global with array field: save and read back through join table.
#[test]
fn global_array_field_save_and_read() {
    let (_tmp, pool, def) = setup_global_with_joins();
    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();

    // Save array data via join table
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

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
    query::set_array_rows(&tx, "_global_homepage", "links", "default", &rows, &links_field.fields, None)
        .expect("Set array rows failed");
    tx.commit().expect("Commit");

    // Read back through get_global (which now calls hydrate_document)
    let conn = pool.get().expect("DB connection");
    let doc = query::get_global(&conn, "homepage", &def, None)
        .expect("Get global failed");

    let links = doc.get("links").expect("links should be populated");
    let links_arr = links.as_array().expect("links should be an array");
    assert_eq!(links_arr.len(), 2);
    assert_eq!(links_arr[0]["url"], "https://example.com");
    assert_eq!(links_arr[0]["label"], "Example");
    assert_eq!(links_arr[1]["url"], "https://rust-lang.org");
    assert_eq!(links_arr[1]["label"], "Rust");
}

/// Global with blocks field: save and read back through join table.
#[test]
fn global_blocks_field_save_and_read() {
    let (_tmp, pool, def) = setup_global_with_joins();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    let blocks = vec![
        serde_json::json!({"_block_type": "paragraph", "text": "Welcome to the homepage"}),
        serde_json::json!({"_block_type": "image", "url": "/hero.jpg"}),
    ];
    query::set_block_rows(&tx, "_global_homepage", "content", "default", &blocks, None)
        .expect("Set block rows failed");
    tx.commit().expect("Commit");

    // Read back
    let conn = pool.get().expect("DB connection");
    let doc = query::get_global(&conn, "homepage", &def, None)
        .expect("Get global failed");

    let content = doc.get("content").expect("content should be populated");
    let content_arr = content.as_array().expect("content should be an array");
    assert_eq!(content_arr.len(), 2);
    assert_eq!(content_arr[0]["_block_type"], "paragraph");
    assert_eq!(content_arr[0]["text"], "Welcome to the homepage");
    assert_eq!(content_arr[1]["_block_type"], "image");
    assert_eq!(content_arr[1]["url"], "/hero.jpg");
}

/// Global with has-many relationship: save and read back through junction table.
#[test]
fn global_has_many_field_save_and_read() {
    let (_tmp, pool, def) = setup_global_with_joins();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    let ids = vec!["post-1".to_string(), "post-2".to_string(), "post-3".to_string()];
    query::set_related_ids(&tx, "_global_homepage", "featured_posts", "default", &ids, None)
        .expect("Set related IDs failed");
    tx.commit().expect("Commit");

    // Read back
    let conn = pool.get().expect("DB connection");
    let doc = query::get_global(&conn, "homepage", &def, None)
        .expect("Get global failed");

    let posts = doc.get("featured_posts").expect("featured_posts should be populated");
    let posts_arr = posts.as_array().expect("featured_posts should be an array");
    assert_eq!(posts_arr.len(), 3);
    assert_eq!(posts_arr[0], "post-1");
    assert_eq!(posts_arr[1], "post-2");
    assert_eq!(posts_arr[2], "post-3");
}

/// save_join_table_data works with global table names (prefixed _global_).
#[test]
fn global_save_join_table_data() {
    let (_tmp, pool, def) = setup_global_with_joins();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    let mut join_data: HashMap<String, serde_json::Value> = HashMap::new();
    join_data.insert("links".to_string(), serde_json::json!([
        {"url": "https://a.com", "label": "A"},
    ]));
    join_data.insert("content".to_string(), serde_json::json!([
        {"_block_type": "paragraph", "text": "Hello"},
    ]));
    join_data.insert("featured_posts".to_string(), serde_json::json!(["p1", "p2"]));

    query::save_join_table_data(&tx, "_global_homepage", &def.fields, "default", &join_data, None)
        .expect("Save join table data failed");
    tx.commit().expect("Commit");

    // Verify everything via get_global (hydration)
    let conn = pool.get().expect("DB connection");
    let doc = query::get_global(&conn, "homepage", &def, None)
        .expect("Get global failed");

    let links = doc.get("links").unwrap().as_array().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["url"], "https://a.com");

    let content = doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["_block_type"], "paragraph");

    let posts = doc.get("featured_posts").unwrap().as_array().unwrap();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0], "p1");
    assert_eq!(posts[1], "p2");
}

/// Updating join table data replaces old data.
#[test]
fn global_join_table_data_replaces_on_update() {
    let (_tmp, pool, def) = setup_global_with_joins();

    let mut conn = pool.get().expect("DB connection");

    // First save
    {
        let tx = conn.transaction().expect("Start transaction");
        let mut join_data: HashMap<String, serde_json::Value> = HashMap::new();
        join_data.insert("links".to_string(), serde_json::json!([
            {"url": "https://old.com", "label": "Old"},
        ]));
        query::save_join_table_data(&tx, "_global_homepage", &def.fields, "default", &join_data, None)
            .expect("Save failed");
        tx.commit().expect("Commit");
    }

    // Second save — should replace
    {
        let tx = conn.transaction().expect("Start transaction");
        let mut join_data: HashMap<String, serde_json::Value> = HashMap::new();
        join_data.insert("links".to_string(), serde_json::json!([
            {"url": "https://new1.com", "label": "New 1"},
            {"url": "https://new2.com", "label": "New 2"},
        ]));
        query::save_join_table_data(&tx, "_global_homepage", &def.fields, "default", &join_data, None)
            .expect("Save failed");
        tx.commit().expect("Commit");
    }

    let conn2 = pool.get().expect("DB connection");
    let doc = query::get_global(&conn2, "homepage", &def, None).expect("Get failed");

    let links = doc.get("links").unwrap().as_array().unwrap();
    assert_eq!(links.len(), 2, "Old data should be replaced by new");
    assert_eq!(links[0]["url"], "https://new1.com");
    assert_eq!(links[1]["url"], "https://new2.com");
}

/// Group fields work correctly in globals using expanded sub-columns (same as collections).
#[test]
fn global_group_field_preserved() {
    let (_tmp, pool, def) = setup_global_with_joins();

    // Update global with group sub-field data (expanded columns)
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "My Homepage".to_string());
    data.insert("seo__meta_title".to_string(), "Home".to_string());
    data.insert("seo__meta_description".to_string(), "Welcome".to_string());
    query::update_global(&tx, "homepage", &def, &data, None)
        .expect("Update failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let doc = query::get_global(&conn, "homepage", &def, None)
        .expect("Get global failed");

    assert_eq!(doc.get_str("title"), Some("My Homepage"));
    // Group field should be hydrated as a nested object from sub-columns
    let seo = doc.get("seo").expect("seo should be present");
    assert!(seo.is_object(), "seo should be an object (reconstructed from sub-columns)");
    assert_eq!(seo.get("meta_title").and_then(|v| v.as_str()), Some("Home"));
    assert_eq!(seo.get("meta_description").and_then(|v| v.as_str()), Some("Welcome"));
}

/// Global with mixed scalar, group, array, blocks, has-many fields all work together.
#[test]
fn global_mixed_fields_coexist() {
    let (_tmp, pool, def) = setup_global_with_joins();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    // Update scalar + group sub-field data
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Homepage".to_string());
    data.insert("seo__meta_title".to_string(), "Home".to_string());
    query::update_global(&tx, "homepage", &def, &data, None)
        .expect("Update failed");

    // Save join table data
    let mut join_data: HashMap<String, serde_json::Value> = HashMap::new();
    join_data.insert("links".to_string(), serde_json::json!([
        {"url": "https://example.com", "label": "Link"},
    ]));
    join_data.insert("content".to_string(), serde_json::json!([
        {"_block_type": "paragraph", "text": "Hello world"},
    ]));
    join_data.insert("featured_posts".to_string(), serde_json::json!(["p1"]));
    query::save_join_table_data(&tx, "_global_homepage", &def.fields, "default", &join_data, None)
        .expect("Save join data failed");

    tx.commit().expect("Commit");

    // Read back — all fields should be populated
    let conn = pool.get().expect("DB connection");
    let doc = query::get_global(&conn, "homepage", &def, None)
        .expect("Get global failed");

    // Scalar
    assert_eq!(doc.get_str("title"), Some("Homepage"));

    // Group (reconstructed as nested object from sub-columns)
    let seo = doc.get("seo").expect("seo should exist");
    assert!(seo.is_object(), "seo should be an object");
    assert_eq!(seo.get("meta_title").and_then(|v| v.as_str()), Some("Home"));

    // Array
    let links = doc.get("links").unwrap().as_array().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["url"], "https://example.com");

    // Blocks
    let content = doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["_block_type"], "paragraph");

    // Has-many
    let posts = doc.get("featured_posts").unwrap().as_array().unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0], "p1");
}

/// Empty arrays/blocks/has-many return empty JSON arrays after hydration.
#[test]
fn global_empty_join_data_returns_empty_arrays() {
    let (_tmp, pool, def) = setup_global_with_joins();

    let conn = pool.get().expect("DB connection");
    let doc = query::get_global(&conn, "homepage", &def, None)
        .expect("Get global failed");

    // All join-table fields should be empty arrays (hydrated but no data)
    let links = doc.get("links").expect("links should exist");
    assert_eq!(links.as_array().unwrap().len(), 0);

    let content = doc.get("content").expect("content should exist");
    assert_eq!(content.as_array().unwrap().len(), 0);

    let posts = doc.get("featured_posts").expect("featured_posts should exist");
    assert_eq!(posts.as_array().unwrap().len(), 0);
}

/// ALTER TABLE for existing globals adds new scalar columns.
#[test]
fn global_alter_table_adds_new_columns() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();

    // First sync: minimal global
    let def_v1 = GlobalDefinition {
        slug: "evolving".to_string(),
        labels: CollectionLabels::default(),
        fields: vec![
            make_field("name", FieldType::Text),
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
    };
    {
        let mut reg = registry.write().unwrap();
        reg.register_global(def_v1.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync v1 failed");

    // Write data
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("name".to_string(), "Test".to_string());
    query::update_global(&tx, "evolving", &def_v1, &data, None).expect("Update v1 failed");
    tx.commit().expect("Commit");

    // Second sync: add a new field
    let def_v2 = GlobalDefinition {
        slug: "evolving".to_string(),
        labels: CollectionLabels::default(),
        fields: vec![
            make_field("name", FieldType::Text),
            make_field("description", FieldType::Textarea),
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
    };
    {
        let mut reg = registry.write().unwrap();
        reg.globals.clear();
        reg.register_global(def_v2.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync v2 failed");

    // Old data should still be there, new column should exist
    let conn = pool.get().expect("DB connection");
    let doc = query::get_global(&conn, "evolving", &def_v2, None).expect("Get failed");
    assert_eq!(doc.get_str("name"), Some("Test"), "Old data should be preserved");
    // New column exists (NULL value for existing row)
    assert!(doc.fields.contains_key("description"), "New column should exist");
}

/// ALTER TABLE for existing globals adds join tables for new array fields.
#[test]
fn global_alter_table_adds_join_tables() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();

    // First sync: scalar-only global
    let def_v1 = GlobalDefinition {
        slug: "growing".to_string(),
        labels: CollectionLabels::default(),
        fields: vec![
            make_field("title", FieldType::Text),
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
    };
    {
        let mut reg = registry.write().unwrap();
        reg.register_global(def_v1);
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync v1 failed");

    // Second sync: add array field
    let def_v2 = GlobalDefinition {
        slug: "growing".to_string(),
        labels: CollectionLabels::default(),
        fields: vec![
            make_field("title", FieldType::Text),
            FieldDefinition {
                name: "items".to_string(),
                field_type: FieldType::Array,
                fields: vec![
                    make_field("label", FieldType::Text),
                ],
                ..Default::default()
            },
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
    };
    {
        let mut reg = registry.write().unwrap();
        reg.globals.clear();
        reg.register_global(def_v2.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync v2 failed");

    // Join table should exist
    let conn = pool.get().expect("DB connection");
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='_global_growing_items'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 1, "Array join table should be created on ALTER");

    // Save and read back array data
    let mut conn2 = pool.get().expect("DB connection");
    let tx = conn2.transaction().expect("Start transaction");
    let items_field = def_v2.fields.iter().find(|f| f.name == "items").unwrap();
    let rows = vec![{
        let mut m = HashMap::new();
        m.insert("label".to_string(), "First".to_string());
        m
    }];
    query::set_array_rows(&tx, "_global_growing", "items", "default", &rows, &items_field.fields, None)
        .expect("Set array rows failed");
    tx.commit().expect("Commit");

    let conn3 = pool.get().expect("DB connection");
    let doc = query::get_global(&conn3, "growing", &def_v2, None).expect("Get failed");
    let items = doc.get("items").unwrap().as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["label"], "First");
}

/// hydrate_document Group guard: when a global stores groups as single JSON columns,
/// hydrate_document must NOT attempt to reconstruct from __-prefixed sub-columns.
#[test]
fn hydrate_document_skips_group_reconstruction_for_globals() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE _global_test (
            id TEXT PRIMARY KEY,
            title TEXT,
            seo TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        INSERT INTO _global_test (id, title, seo, created_at, updated_at)
        VALUES ('default', 'Test', '{\"meta_title\":\"Hello\"}', '2024-01-01', '2024-01-01');"
    ).unwrap();

    let fields = vec![
        make_field("title", FieldType::Text),
        FieldDefinition {
            name: "seo".to_string(),
            field_type: FieldType::Group,
            fields: vec![
                make_field("meta_title", FieldType::Text),
            ],
            ..Default::default()
        },
    ];

    // Simulate what get_global does: read the row, then hydrate
    let mut doc = conn.query_row(
        "SELECT id, title, seo, created_at, updated_at FROM _global_test WHERE id = 'default'",
        [],
        |row| {
            crap_cms::db::document::row_to_document(row, &[
                "id".to_string(), "title".to_string(), "seo".to_string(),
                "created_at".to_string(), "updated_at".to_string(),
            ])
        },
    ).unwrap();

    // Hydrate should NOT touch the group field (no seo__meta_title sub-column exists)
    query::hydrate_document(&conn, "_global_test", &fields, &mut doc, None, None).unwrap();

    // Group field should still be the raw JSON string, NOT reconstructed
    assert_eq!(doc.get_str("seo"), Some("{\"meta_title\":\"Hello\"}"));
    assert_eq!(doc.get_str("title"), Some("Test"));
}

/// hydrate_document Group reconstruction still works for collections
/// (where __-prefixed sub-columns DO exist).
#[test]
fn hydrate_document_reconstructs_group_for_collections() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE pages (
            id TEXT PRIMARY KEY,
            title TEXT,
            seo__meta_title TEXT,
            seo__meta_description TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        INSERT INTO pages (id, title, seo__meta_title, seo__meta_description, created_at, updated_at)
        VALUES ('p1', 'Page', 'Page Title', 'Page Desc', '2024-01-01', '2024-01-01');"
    ).unwrap();

    let fields = vec![
        make_field("title", FieldType::Text),
        FieldDefinition {
            name: "seo".to_string(),
            field_type: FieldType::Group,
            fields: vec![
                make_field("meta_title", FieldType::Text),
                make_field("meta_description", FieldType::Textarea),
            ],
            ..Default::default()
        },
    ];

    let mut doc = conn.query_row(
        "SELECT id, title, seo__meta_title, seo__meta_description, created_at, updated_at FROM pages WHERE id = 'p1'",
        [],
        |row| {
            crap_cms::db::document::row_to_document(row, &[
                "id".to_string(), "title".to_string(),
                "seo__meta_title".to_string(), "seo__meta_description".to_string(),
                "created_at".to_string(), "updated_at".to_string(),
            ])
        },
    ).unwrap();

    // Before hydration: sub-columns are separate keys
    assert!(doc.fields.contains_key("seo__meta_title"));

    query::hydrate_document(&conn, "pages", &fields, &mut doc, None, None).unwrap();

    // After hydration: reconstructed into nested object
    assert!(!doc.fields.contains_key("seo__meta_title"), "Sub-column should be removed");
    let seo = doc.get("seo").expect("seo should exist");
    let seo_obj = seo.as_object().expect("seo should be an object");
    assert_eq!(seo_obj.get("meta_title").unwrap(), "Page Title");
    assert_eq!(seo_obj.get("meta_description").unwrap(), "Page Desc");
}

// ── Group + Locale Tests (Collections) ────────────────────────────────────────

fn make_localized_group_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "pages_l10n".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("title", FieldType::Text),
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "meta_title".to_string(),
                        localized: true,
                        ..Default::default()
                    },
                    FieldDefinition {
                        name: "meta_description".to_string(),
                        localized: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    }
}

fn locale_config() -> LocaleConfig {
    LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}

/// Collection: localized group creates seo__meta_title__en, seo__meta_title__de columns.
#[test]
fn collection_localized_group_migration_creates_locale_columns() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_localized_group_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &locale_config()).expect("Sync");

    let conn = pool.get().unwrap();
    let mut stmt = conn.prepare("PRAGMA table_info(pages_l10n)").unwrap();
    let columns: HashSet<String> = stmt.query_map([], |row| {
        row.get::<_, String>(1)
    }).unwrap().filter_map(|r| r.ok()).collect();

    assert!(columns.contains("seo__meta_title__en"), "Should have en locale column for group sub-field");
    assert!(columns.contains("seo__meta_title__de"), "Should have de locale column for group sub-field");
    assert!(columns.contains("seo__meta_description__en"));
    assert!(columns.contains("seo__meta_description__de"));
    assert!(!columns.contains("seo__meta_title"), "Should NOT have non-localized group sub-column");
    assert!(!columns.contains("seo"), "Should NOT have single group column");
}

/// Collection: write and read localized group sub-fields per locale.
#[test]
fn collection_localized_group_write_and_read() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_localized_group_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &locale_config()).expect("Sync");

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config(),
    };

    // Create with English group data
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test Page".to_string());
    data.insert("seo__meta_title".to_string(), "EN SEO Title".to_string());
    data.insert("seo__meta_description".to_string(), "EN SEO Desc".to_string());

    let conn = pool.get().unwrap();
    let doc = query::create(&conn, "pages_l10n", &def, &data, Some(&en_ctx)).expect("Create");

    // Update with German group data
    let mut de_data = HashMap::new();
    de_data.insert("seo__meta_title".to_string(), "DE SEO Titel".to_string());
    de_data.insert("seo__meta_description".to_string(), "DE SEO Beschreibung".to_string());
    query::update(&conn, "pages_l10n", &def, &doc.id, &de_data, Some(&de_ctx)).expect("Update DE");

    // Read English — hydrated into nested seo object
    let en_doc = query::find_by_id(&conn, "pages_l10n", &def, &doc.id, Some(&en_ctx)).unwrap().unwrap();
    let en_seo = en_doc.fields.get("seo").expect("seo should exist");
    assert_eq!(en_seo.get("meta_title").and_then(|v| v.as_str()), Some("EN SEO Title"));
    assert_eq!(en_seo.get("meta_description").and_then(|v| v.as_str()), Some("EN SEO Desc"));

    // Read German — should get DE values
    let de_doc = query::find_by_id(&conn, "pages_l10n", &def, &doc.id, Some(&de_ctx)).unwrap().unwrap();
    let de_seo = de_doc.fields.get("seo").expect("seo should exist");
    assert_eq!(de_seo.get("meta_title").and_then(|v| v.as_str()), Some("DE SEO Titel"));
    assert_eq!(de_seo.get("meta_description").and_then(|v| v.as_str()), Some("DE SEO Beschreibung"));
}

// ── Group + Locale Tests (Globals) ────────────────────────────────────────────

fn make_global_with_localized_group() -> GlobalDefinition {
    GlobalDefinition {
        slug: "site_l10n".to_string(),
        labels: CollectionLabels::default(),
        fields: vec![
            make_field("site_name", FieldType::Text),
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "meta_title".to_string(),
                        localized: true,
                        ..Default::default()
                    },
                    FieldDefinition {
                        name: "meta_description".to_string(),
                        localized: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
    }
}

/// Global: localized group creates seo__meta_title__en, seo__meta_title__de columns.
#[test]
fn global_localized_group_migration_creates_locale_columns() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_global_with_localized_group();
    {
        let mut reg = registry.write().unwrap();
        reg.register_global(def.clone());
    }
    migrate::sync_all(&pool, &registry, &locale_config()).expect("Sync");

    let conn = pool.get().unwrap();
    let mut stmt = conn.prepare("PRAGMA table_info(_global_site_l10n)").unwrap();
    let columns: HashSet<String> = stmt.query_map([], |row| {
        row.get::<_, String>(1)
    }).unwrap().filter_map(|r| r.ok()).collect();

    assert!(columns.contains("seo__meta_title__en"), "Should have en locale column for group sub-field");
    assert!(columns.contains("seo__meta_title__de"), "Should have de locale column for group sub-field");
    assert!(columns.contains("seo__meta_description__en"));
    assert!(columns.contains("seo__meta_description__de"));
    assert!(!columns.contains("seo__meta_title"), "Should NOT have non-localized group sub-column");
    assert!(!columns.contains("seo"), "Should NOT have single group column");
}

/// Global: write and read localized group sub-fields per locale.
#[test]
fn global_localized_group_write_and_read() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_global_with_localized_group();
    {
        let mut reg = registry.write().unwrap();
        reg.register_global(def.clone());
    }
    migrate::sync_all(&pool, &registry, &locale_config()).expect("Sync");

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config(),
    };

    // Update with English group data
    let mut data = HashMap::new();
    data.insert("site_name".to_string(), "My Site".to_string());
    data.insert("seo__meta_title".to_string(), "EN Title".to_string());
    data.insert("seo__meta_description".to_string(), "EN Desc".to_string());

    let conn = pool.get().unwrap();
    query::update_global(&conn, "site_l10n", &def, &data, Some(&en_ctx)).expect("Update EN");

    // Update with German group data
    let mut de_data = HashMap::new();
    de_data.insert("seo__meta_title".to_string(), "DE Titel".to_string());
    de_data.insert("seo__meta_description".to_string(), "DE Beschreibung".to_string());
    query::update_global(&conn, "site_l10n", &def, &de_data, Some(&de_ctx)).expect("Update DE");

    // Read English (hydrated into nested seo object)
    let en_doc = query::get_global(&conn, "site_l10n", &def, Some(&en_ctx)).expect("Get EN");
    let en_seo = en_doc.fields.get("seo").expect("seo should exist");
    assert_eq!(en_seo.get("meta_title").and_then(|v| v.as_str()), Some("EN Title"));
    assert_eq!(en_seo.get("meta_description").and_then(|v| v.as_str()), Some("EN Desc"));

    // Read German
    let de_doc = query::get_global(&conn, "site_l10n", &def, Some(&de_ctx)).expect("Get DE");
    let de_seo = de_doc.fields.get("seo").expect("seo should exist");
    assert_eq!(de_seo.get("meta_title").and_then(|v| v.as_str()), Some("DE Titel"));
    assert_eq!(de_seo.get("meta_description").and_then(|v| v.as_str()), Some("DE Beschreibung"));
}

/// Global: locale fallback for group sub-fields.
#[test]
fn global_localized_group_fallback() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_global_with_localized_group();
    {
        let mut reg = registry.write().unwrap();
        reg.register_global(def.clone());
    }
    migrate::sync_all(&pool, &registry, &locale_config()).expect("Sync");

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config(),
    };

    // Only set English data
    let mut data = HashMap::new();
    data.insert("seo__meta_title".to_string(), "EN Title".to_string());
    let conn = pool.get().unwrap();
    query::update_global(&conn, "site_l10n", &def, &data, Some(&en_ctx)).expect("Update EN");

    // Read German — should fall back to English (fallback=true)
    let de_doc = query::get_global(&conn, "site_l10n", &def, Some(&de_ctx)).expect("Get DE");
    let de_seo = de_doc.fields.get("seo").expect("seo should exist");
    assert_eq!(de_seo.get("meta_title").and_then(|v| v.as_str()), Some("EN Title"), "Should fall back to EN");
}

/// update_global skips join-table fields (no column for them in parent table).
#[test]
fn global_update_ignores_join_table_field_values() {
    let (_tmp, pool, def) = setup_global_with_joins();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    // Include both scalar data and array/blocks data in the update map.
    // The array/blocks values should be ignored by update_global (no parent column).
    let mut data = HashMap::new();
    data.insert("title".to_string(), "My Title".to_string());
    // These should not cause SQL errors even though no column exists:
    data.insert("links".to_string(), "should be ignored".to_string());
    data.insert("content".to_string(), "should be ignored".to_string());
    data.insert("featured_posts".to_string(), "should be ignored".to_string());

    let doc = query::update_global(&tx, "homepage", &def, &data, None)
        .expect("Update should succeed despite join-table field values in data");
    tx.commit().expect("Commit");

    assert_eq!(doc.get_str("title"), Some("My Title"));
}

// ── ALTER TABLE Group Field Tests ─────────────────────────────────────────────

/// Collection ALTER TABLE: adding a group field creates sub-columns.
#[test]
fn collection_alter_adds_group_sub_columns() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();

    // First sync: simple collection
    let mut def = CollectionDefinition {
        slug: "articles".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("title", FieldType::Text),
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    };
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync v1");

    // Write initial data
    let conn = pool.get().unwrap();
    let mut data = HashMap::new();
    data.insert("title".to_string(), "My Article".to_string());
    let doc = query::create(&conn, "articles", &def, &data, None).expect("Create");

    // Second sync: add a group field
    def.fields.push(FieldDefinition {
        name: "seo".to_string(),
        field_type: FieldType::Group,
        fields: vec![
            make_field("meta_title", FieldType::Text),
            make_field("meta_description", FieldType::Textarea),
        ],
        ..Default::default()
    });
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync v2");

    // Verify sub-columns exist
    let mut stmt = conn.prepare("PRAGMA table_info(articles)").unwrap();
    let columns: HashSet<String> = stmt.query_map([], |row| {
        row.get::<_, String>(1)
    }).unwrap().filter_map(|r| r.ok()).collect();

    assert!(columns.contains("seo__meta_title"), "Should have seo__meta_title sub-column");
    assert!(columns.contains("seo__meta_description"), "Should have seo__meta_description sub-column");
    assert!(!columns.contains("seo"), "Should NOT have single seo column");

    // Old data preserved, new sub-columns are NULL
    let old_doc = query::find_by_id(&conn, "articles", &def, &doc.id, None).unwrap().unwrap();
    assert_eq!(old_doc.get_str("title"), Some("My Article"));

    // Write new data with group sub-fields
    let mut new_data = HashMap::new();
    new_data.insert("seo__meta_title".to_string(), "SEO Title".to_string());
    new_data.insert("seo__meta_description".to_string(), "SEO Desc".to_string());
    query::update(&conn, "articles", &def, &doc.id, &new_data, None).expect("Update");

    let updated = query::find_by_id(&conn, "articles", &def, &doc.id, None).unwrap().unwrap();
    let seo = updated.fields.get("seo").expect("seo should exist after hydration");
    assert_eq!(seo.get("meta_title").and_then(|v| v.as_str()), Some("SEO Title"));
    assert_eq!(seo.get("meta_description").and_then(|v| v.as_str()), Some("SEO Desc"));
    assert_eq!(updated.get_str("title"), Some("My Article"), "Old data preserved");
}

/// Global ALTER TABLE: adding a group field creates sub-columns.
#[test]
fn global_alter_adds_group_sub_columns() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();

    // First sync: simple global
    let def_v1 = GlobalDefinition {
        slug: "settings".to_string(),
        labels: CollectionLabels::default(),
        fields: vec![
            make_field("site_name", FieldType::Text),
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
    };
    {
        let mut reg = registry.write().unwrap();
        reg.register_global(def_v1.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync v1");

    // Write initial data
    let conn = pool.get().unwrap();
    let mut data = HashMap::new();
    data.insert("site_name".to_string(), "My Site".to_string());
    query::update_global(&conn, "settings", &def_v1, &data, None).expect("Update v1");

    // Second sync: add a group field
    let def_v2 = GlobalDefinition {
        slug: "settings".to_string(),
        labels: CollectionLabels::default(),
        fields: vec![
            make_field("site_name", FieldType::Text),
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    make_field("meta_title", FieldType::Text),
                    make_field("og_image", FieldType::Text),
                ],
                ..Default::default()
            },
        ],
        hooks: CollectionHooks::default(),
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
    };
    {
        let mut reg = registry.write().unwrap();
        reg.globals.clear();
        reg.register_global(def_v2.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync v2");

    // Verify sub-columns exist
    let mut stmt = conn.prepare("PRAGMA table_info(_global_settings)").unwrap();
    let columns: HashSet<String> = stmt.query_map([], |row| {
        row.get::<_, String>(1)
    }).unwrap().filter_map(|r| r.ok()).collect();

    assert!(columns.contains("seo__meta_title"), "Should have seo__meta_title sub-column");
    assert!(columns.contains("seo__og_image"), "Should have seo__og_image sub-column");
    assert!(!columns.contains("seo"), "Should NOT have single seo column");

    // Old data preserved
    let doc = query::get_global(&conn, "settings", &def_v2, None).expect("Get");
    assert_eq!(doc.get_str("site_name"), Some("My Site"), "Old data preserved");

    // Write group data
    let mut new_data = HashMap::new();
    new_data.insert("seo__meta_title".to_string(), "Global SEO".to_string());
    new_data.insert("seo__og_image".to_string(), "/og.png".to_string());
    query::update_global(&conn, "settings", &def_v2, &new_data, None).expect("Update v2");

    let updated = query::get_global(&conn, "settings", &def_v2, None).expect("Get v2");
    let seo = updated.fields.get("seo").expect("seo should exist after hydration");
    assert_eq!(seo.get("meta_title").and_then(|v| v.as_str()), Some("Global SEO"));
    assert_eq!(seo.get("og_image").and_then(|v| v.as_str()), Some("/og.png"));
}

/// Collection ALTER TABLE: adding localized group sub-fields creates locale columns.
#[test]
fn collection_alter_adds_localized_group_columns() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let lc = locale_config();

    // First sync: collection with non-localized group
    let mut def = CollectionDefinition {
        slug: "pages_alter".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("title", FieldType::Text),
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    make_field("meta_title", FieldType::Text),
                ],
                ..Default::default()
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    };
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &lc).expect("Sync v1");

    // Verify non-localized sub-column
    let conn = pool.get().unwrap();
    let mut stmt = conn.prepare("PRAGMA table_info(pages_alter)").unwrap();
    let columns_v1: HashSet<String> = stmt.query_map([], |row| {
        row.get::<_, String>(1)
    }).unwrap().filter_map(|r| r.ok()).collect();
    assert!(columns_v1.contains("seo__meta_title"), "Non-localized sub-column should exist");
    assert!(!columns_v1.contains("seo__meta_title__en"), "Locale columns should not exist yet");

    // Second sync: add a new localized sub-field to the group
    def.fields[1].fields.push(FieldDefinition {
        name: "og_description".to_string(),
        localized: true,
        ..Default::default()
    });
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &lc).expect("Sync v2");

    // Verify new locale columns were added
    let mut stmt2 = conn.prepare("PRAGMA table_info(pages_alter)").unwrap();
    let columns_v2: HashSet<String> = stmt2.query_map([], |row| {
        row.get::<_, String>(1)
    }).unwrap().filter_map(|r| r.ok()).collect();

    assert!(columns_v2.contains("seo__meta_title"), "Original sub-column preserved");
    assert!(columns_v2.contains("seo__og_description__en"), "EN locale column added");
    assert!(columns_v2.contains("seo__og_description__de"), "DE locale column added");
    assert!(!columns_v2.contains("seo__og_description"), "Non-localized column should NOT exist");
}

// ── Dot-notation / sub-field filter integration tests ───────────────────────

/// Build a collection with array, blocks, group, and has-many relationship fields
/// for testing dot-notation sub-field filtering.
fn make_filterable_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "products".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![
            make_field("name", FieldType::Text),
            // Group field
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    make_field("meta_title", FieldType::Text),
                    make_field("meta_description", FieldType::Text),
                ],
                ..make_field("seo", FieldType::Group)
            },
            // Array field with sub-fields (including a Group sub-field)
            FieldDefinition {
                name: "variants".to_string(),
                field_type: FieldType::Array,
                fields: vec![
                    make_field("sku", FieldType::Text),
                    make_field("color", FieldType::Text),
                    make_field("size", FieldType::Text),
                    FieldDefinition {
                        name: "dimensions".to_string(),
                        field_type: FieldType::Group,
                        fields: vec![
                            make_field("width", FieldType::Text),
                            make_field("height", FieldType::Text),
                        ],
                        ..make_field("dimensions", FieldType::Group)
                    },
                ],
                ..make_field("variants", FieldType::Array)
            },
            // Blocks field
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Blocks,
                blocks: vec![
                    BlockDefinition {
                        block_type: "text".to_string(),
                        fields: vec![make_field("body", FieldType::Textarea)],
                        ..Default::default()
                    },
                    BlockDefinition {
                        block_type: "image".to_string(),
                        fields: vec![
                            make_field("url", FieldType::Text),
                            make_field("alt", FieldType::Text),
                        ],
                        ..Default::default()
                    },
                    BlockDefinition {
                        block_type: "section".to_string(),
                        fields: vec![
                            make_field("heading", FieldType::Text),
                            FieldDefinition {
                                name: "meta".to_string(),
                                field_type: FieldType::Group,
                                fields: vec![
                                    make_field("author", FieldType::Text),
                                ],
                                ..make_field("meta", FieldType::Group)
                            },
                        ],
                        ..Default::default()
                    },
                ],
                ..make_field("content", FieldType::Blocks)
            },
            // Has-many relationship
            FieldDefinition {
                name: "tags".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig {
                    collection: "product_tags".to_string(),
                    has_many: true,
                    max_depth: None,
                    polymorphic: vec![],
                }),
                ..make_field("tags", FieldType::Relationship)
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    }
}

fn setup_filterable() -> (tempfile::TempDir, crap_cms::db::DbPool, CollectionDefinition) {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_filterable_def();
    let tags_def = CollectionDefinition {
        slug: "product_tags".to_string(),
        labels: CollectionLabels::default(),
        timestamps: true,
        fields: vec![make_field("label", FieldType::Text)],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
        mcp: Default::default(),
        live: None,
        versions: None,
            indexes: Vec::new(),
    };
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
        reg.register_collection(tags_def);
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");
    (_tmp, pool, def)
}

/// Seed two products with different array/block/relationship data.
fn seed_filterable_products(pool: &crap_cms::db::DbPool, def: &CollectionDefinition) -> (String, String) {
    let variants_field = def.fields.iter().find(|f| f.name == "variants").unwrap();

    // Product 1: "Widget" with red variant, text block, tagged "sale"
    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let mut data1 = HashMap::new();
    data1.insert("name".to_string(), "Widget".to_string());
    data1.insert("seo__meta_title".to_string(), "Buy Widget".to_string());
    data1.insert("seo__meta_description".to_string(), "Best widget".to_string());
    let doc1 = query::create(&tx, "products", def, &data1, None).unwrap();
    let id1 = doc1.id.clone();

    // Array rows for product 1
    let rows1 = vec![
        HashMap::from([
            ("sku".to_string(), "W-001".to_string()),
            ("color".to_string(), "red".to_string()),
            ("size".to_string(), "large".to_string()),
            ("dimensions".to_string(), r#"{"width":"10","height":"20"}"#.to_string()),
        ]),
    ];
    query::set_array_rows(&tx, "products", "variants", &id1, &rows1, &variants_field.fields, None).unwrap();

    // Block rows for product 1
    let blocks1 = vec![
        serde_json::json!({"_block_type": "text", "body": "Widget description here"}),
        serde_json::json!({"_block_type": "image", "url": "/widget.png", "alt": "Widget photo"}),
    ];
    query::set_block_rows(&tx, "products", "content", &id1, &blocks1, None).unwrap();

    // Relationship for product 1
    query::set_related_ids(&tx, "products", "tags", &id1, &["tag-sale".to_string()], None).unwrap();

    tx.commit().unwrap();

    // Product 2: "Gadget" with blue variant, section block, tagged "new"
    let mut conn2 = pool.get().unwrap();
    let tx2 = conn2.transaction().unwrap();
    let mut data2 = HashMap::new();
    data2.insert("name".to_string(), "Gadget".to_string());
    data2.insert("seo__meta_title".to_string(), "Buy Gadget".to_string());
    data2.insert("seo__meta_description".to_string(), "Cool gadget".to_string());
    let doc2 = query::create(&tx2, "products", def, &data2, None).unwrap();
    let id2 = doc2.id.clone();

    let rows2 = vec![
        HashMap::from([
            ("sku".to_string(), "G-001".to_string()),
            ("color".to_string(), "blue".to_string()),
            ("size".to_string(), "small".to_string()),
            ("dimensions".to_string(), r#"{"width":"5","height":"15"}"#.to_string()),
        ]),
    ];
    query::set_array_rows(&tx2, "products", "variants", &id2, &rows2, &variants_field.fields, None).unwrap();

    let blocks2 = vec![
        serde_json::json!({"_block_type": "section", "heading": "About Gadget", "meta": {"author": "Alice"}}),
    ];
    query::set_block_rows(&tx2, "products", "content", &id2, &blocks2, None).unwrap();

    query::set_related_ids(&tx2, "products", "tags", &id2, &["tag-new".to_string()], None).unwrap();

    tx2.commit().unwrap();

    (id1, id2)
}

#[test]
fn filter_array_subfield() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by array sub-field: variants.color = "red"
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "variants.color".to_string(),
            op: query::FilterOp::Equals("red".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Widget"));
}

#[test]
fn filter_array_subfield_contains() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by array sub-field: variants.sku contains "G-"
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "variants.sku".to_string(),
            op: query::FilterOp::Contains("G-".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Gadget"));
}

#[test]
fn filter_array_subfield_size() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by array sub-field: variants.size = "large"
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "variants.size".to_string(),
            op: query::FilterOp::Equals("large".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Widget"));
}

#[test]
fn filter_block_subfield() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by block sub-field: content.body contains "description"
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "content.body".to_string(),
            op: query::FilterOp::Contains("description".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Widget"));
}

#[test]
fn filter_block_type() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by block type: content._block_type = "section"
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "content._block_type".to_string(),
            op: query::FilterOp::Equals("section".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Gadget"));
}

#[test]
fn filter_block_group_subfield() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by group-in-block: content.meta.author = "Alice"
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "content.meta.author".to_string(),
            op: query::FilterOp::Equals("Alice".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Gadget"));
}

#[test]
fn filter_has_many_relationship() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by has-many relationship: tags.id = "tag-sale"
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "tags.id".to_string(),
            op: query::FilterOp::Equals("tag-sale".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Widget"));
}

#[test]
fn filter_group_dot_notation_normalized() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Dot-notation for groups: seo.meta_title gets normalized to seo__meta_title
    // by normalize_filter_fields, which the API/Lua layer calls before find().
    // Here we simulate that normalization.
    let mut filters = vec![query::FilterClause::Single(query::Filter {
        field: "seo.meta_title".to_string(),
        op: query::FilterOp::Equals("Buy Widget".to_string()),
    })];
    query::filter::normalize_filter_fields(&mut filters, &def.fields);

    let q = query::FindQuery {
        filters,
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Widget"));
}

#[test]
fn filter_subquery_combined_with_column_filter() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Combine a regular column filter with a subquery filter:
    // name = "Widget" AND variants.color = "red"
    let q = query::FindQuery {
        filters: vec![
            query::FilterClause::Single(query::Filter {
                field: "name".to_string(),
                op: query::FilterOp::Equals("Widget".to_string()),
            }),
            query::FilterClause::Single(query::Filter {
                field: "variants.color".to_string(),
                op: query::FilterOp::Equals("red".to_string()),
            }),
        ],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Widget"));

    // Non-matching combination: name = "Widget" AND variants.color = "blue" → 0 results
    let q2 = query::FindQuery {
        filters: vec![
            query::FilterClause::Single(query::Filter {
                field: "name".to_string(),
                op: query::FilterOp::Equals("Widget".to_string()),
            }),
            query::FilterClause::Single(query::Filter {
                field: "variants.color".to_string(),
                op: query::FilterOp::Equals("blue".to_string()),
            }),
        ],
        ..Default::default()
    };
    let docs2 = ops::find_documents(&pool, "products", &def, &q2, None).unwrap();
    assert_eq!(docs2.len(), 0);
}

#[test]
fn filter_or_with_subquery() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // OR group with subquery filters:
    // variants.color = "red" OR content._block_type = "section"
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Or(vec![
            vec![query::Filter {
                field: "variants.color".to_string(),
                op: query::FilterOp::Equals("red".to_string()),
            }],
            vec![query::Filter {
                field: "content._block_type".to_string(),
                op: query::FilterOp::Equals("section".to_string()),
            }],
        ])],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 2); // Both products match one of the conditions
}

#[test]
fn filter_subquery_no_match() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter that matches nothing
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "variants.color".to_string(),
            op: query::FilterOp::Equals("green".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 0);
}

#[test]
fn filter_count_with_subquery() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Count with subquery filter
    let filters = vec![query::FilterClause::Single(query::Filter {
        field: "content._block_type".to_string(),
        op: query::FilterOp::Equals("text".to_string()),
    })];
    let count = ops::count_documents(&pool, "products", &def, &filters, None).unwrap();
    assert_eq!(count, 1); // Only Widget has a text block
}

#[test]
fn filter_rejects_invalid_dot_prefix() {
    let (_tmp, pool, def) = setup_filterable();

    // An invalid prefix (no such array/block/relationship field) should be rejected
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "nonexistent.field".to_string(),
            op: query::FilterOp::Equals("x".to_string()),
        })],
        ..Default::default()
    };
    let result = ops::find_documents(&pool, "products", &def, &q, None);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Invalid field"));
}

#[test]
fn filter_array_group_subfield() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by group-in-array: variants.dimensions.width = "10" (Widget)
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "variants.dimensions.width".to_string(),
            op: query::FilterOp::Equals("10".to_string()),
        })],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Widget"));

    // Filter by group-in-array: variants.dimensions.height = "15" (Gadget)
    let q2 = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "variants.dimensions.height".to_string(),
            op: query::FilterOp::Equals("15".to_string()),
        })],
        ..Default::default()
    };
    let docs2 = ops::find_documents(&pool, "products", &def, &q2, None).unwrap();
    assert_eq!(docs2.len(), 1);
    assert_eq!(docs2[0].get_str("name"), Some("Gadget"));
}
