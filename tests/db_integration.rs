use std::collections::HashMap;

use crap_cms::config::CrapConfig;
use crap_cms::core::collection::{
    CollectionAccess, CollectionAdmin, CollectionDefinition, CollectionHooks, CollectionLabels,
};
use crap_cms::core::field::{FieldAccess, FieldAdmin, FieldDefinition, FieldHooks, FieldType};
use crap_cms::core::Registry;
use crap_cms::db::{migrate, ops, pool, query};

fn make_posts_def() -> CollectionDefinition {
    CollectionDefinition {
        slug: "posts".to_string(),
        labels: CollectionLabels {
            singular: Some("Post".to_string()),
            plural: Some("Posts".to_string()),
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
            },
        ],
        admin: CollectionAdmin::default(),
        hooks: CollectionHooks::default(),
        auth: None,
        upload: None,
        access: CollectionAccess::default(),
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
    migrate::sync_all(&pool, &registry).expect("Failed to sync schema");

    // Create
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Hello World".to_string());
    data.insert("status".to_string(), "published".to_string());

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let doc = query::create(&tx, "posts", &def, &data).expect("Failed to create document");
    tx.commit().expect("Commit");

    assert_eq!(doc.get_str("title"), Some("Hello World"));
    assert_eq!(doc.get_str("status"), Some("published"));
    assert!(doc.created_at.is_some());
    let doc_id = doc.id.clone();

    // Read
    let found = ops::find_document_by_id(&pool, "posts", &def, &doc_id)
        .expect("Failed to find document")
        .expect("Document not found");
    assert_eq!(found.id, doc_id);
    assert_eq!(found.get_str("title"), Some("Hello World"));

    // List
    let all = ops::find_documents(&pool, "posts", &def, &query::FindQuery::default())
        .expect("Failed to list documents");
    assert_eq!(all.len(), 1);

    // Update
    let mut update_data = HashMap::new();
    update_data.insert("title".to_string(), "Updated Title".to_string());
    update_data.insert("status".to_string(), "draft".to_string());

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let updated = query::update(&tx, "posts", &def, &doc_id, &update_data)
        .expect("Failed to update document");
    tx.commit().expect("Commit");
    assert_eq!(updated.get_str("title"), Some("Updated Title"));

    // Delete
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    query::delete(&tx, "posts", &doc_id).expect("Failed to delete document");
    tx.commit().expect("Commit");

    let deleted = ops::find_document_by_id(&pool, "posts", &def, &doc_id)
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
    migrate::sync_all(&pool, &registry).expect("First sync failed");

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
    });
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry).expect("Second sync failed");

    // Verify we can use the new column
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    data.insert("body".to_string(), "Some body text".to_string());

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let doc = query::create(&tx, "posts", &def, &data).expect("Failed to create");
    tx.commit().expect("Commit");
    assert_eq!(doc.get_str("body"), Some("Some body text"));
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
    migrate::sync_all(&pool, &registry).expect("Sync failed");

    // Insert 3 documents
    for i in 0..3 {
        let mut data = HashMap::new();
        data.insert("title".to_string(), format!("Post {}", i));
        let mut conn = pool.get().expect("DB connection");
        let tx = conn.transaction().expect("Start transaction");
        query::create(&tx, "posts", &def, &data).expect("Create failed");
        tx.commit().expect("Commit");
    }

    let total = ops::count_documents(&pool, "posts", &def, &[]).expect("Count failed");
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
    migrate::sync_all(&pool, &registry).expect("Sync failed");

    let find_query = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "nonexistent".to_string(),
            op: query::FilterOp::Equals("test".to_string()),
        })],
        ..Default::default()
    };

    let result = ops::find_documents(&pool, "posts", &def, &find_query);
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
    migrate::sync_all(&pool, &registry).expect("Sync failed");

    let find_query = query::FindQuery {
        order_by: Some("nonexistent".to_string()),
        ..Default::default()
    };

    let result = ops::find_documents(&pool, "posts", &def, &find_query);
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
    migrate::sync_all(&pool, &registry).expect("Sync failed");

    let find_query = query::FindQuery {
        filters: vec![query::FilterClause::Single(query::Filter {
            field: "1=1; DROP TABLE posts; --".to_string(),
            op: query::FilterOp::Equals("x".to_string()),
        })],
        ..Default::default()
    };

    let result = ops::find_documents(&pool, "posts", &def, &find_query);
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
    migrate::sync_all(&pool, &registry).expect("Sync failed");

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
        query::create(&tx, "posts", &def, &data).expect("Create failed");
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
    let docs = ops::find_documents(&pool, "posts", &def, &q).expect("Find failed");
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
    let docs = ops::find_documents(&pool, "posts", &def, &q).expect("Find failed");
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
    let docs = ops::find_documents(&pool, "posts", &def, &q).expect("Find failed");
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
    let docs = ops::find_documents(&pool, "posts", &def, &q).expect("Find failed");
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
    let docs = ops::find_documents(&pool, "posts", &def, &q).expect("Find failed");
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
    let docs = ops::find_documents(&pool, "posts", &def, &q).expect("Find failed");
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
    let docs = ops::find_documents(&pool, "posts", &def, &q).expect("Find failed");
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
    let docs = ops::find_documents(&pool, "posts", &def, &q).expect("Find failed");
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
    let docs = ops::find_documents(&pool, "posts", &def, &q).expect("Find failed");
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
    let docs = ops::find_documents(&pool, "posts", &def, &q).expect("Find failed");
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
    let docs = ops::find_documents(&pool, "posts", &def, &q).expect("Find failed");
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
    let docs = ops::find_documents(&pool, "posts", &def, &q).expect("Find failed");
    // Only "Epsilon Post" has NULL status
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("title"), Some("Epsilon Post"));
}

#[test]
fn filter_or_clause() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery {
        filters: vec![query::FilterClause::Or(vec![
            query::Filter {
                field: "title".to_string(),
                op: query::FilterOp::Contains("Alpha".to_string()),
            },
            query::Filter {
                field: "title".to_string(),
                op: query::FilterOp::Contains("Gamma".to_string()),
            },
        ])],
        ..Default::default()
    };
    let docs = ops::find_documents(&pool, "posts", &def, &q).expect("Find failed");
    assert_eq!(docs.len(), 2);
    let titles: Vec<_> = docs.iter().filter_map(|d| d.get_str("title")).collect();
    assert!(titles.contains(&"Alpha Post"));
    assert!(titles.contains(&"Gamma Post"));
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
    let docs = ops::find_documents(&pool, "posts", &def, &q).expect("Find failed");
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
    let count = ops::count_documents(&pool, "posts", &def, &filters).expect("Count failed");
    assert_eq!(count, 2);
}
