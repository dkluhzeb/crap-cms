use std::collections::HashMap;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::Registry;
use crap_cms::core::collection::{CollectionDefinition, Labels};
use crap_cms::core::field::{
    BlockDefinition, FieldDefinition, FieldType, LocalizedString, RelationshipConfig,
};
use crap_cms::db::query::{LocaleContext, LocaleMode};
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

// ── Filter operator tests ────────────────────────────────────────────────────

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

#[test]
fn filter_equals() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::Equals("published".to_string()),
        })])
        .build();
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(docs.len(), 2);
    for doc in &docs {
        assert_eq!(doc.get_str("status"), Some("published"));
    }
}

#[test]
fn filter_not_equals() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::NotEquals("published".to_string()),
        })])
        .build();
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
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::Contains("eta".to_string()),
        })])
        .build();
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("title"), Some("Beta Post"));
}

#[test]
fn filter_like() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::Like("A%".to_string()),
        })])
        .build();
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("title"), Some("Alpha Post"));
}

#[test]
fn filter_greater_than() {
    let (_tmp, pool, def) = seed_posts();
    // SQLite text comparison: "D" < "Delta Post", "E" < "Epsilon Post", "G" < "Gamma Post"
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::GreaterThan("D".to_string()),
        })])
        .build();
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
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::LessThan("C".to_string()),
        })])
        .build();
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
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::GreaterThanOrEqual("Gamma Post".to_string()),
        })])
        .build();
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
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::LessThanOrEqual("Beta Post".to_string()),
        })])
        .build();
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
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::In(vec!["draft".to_string(), "archived".to_string()]),
        })])
        .build();
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert_eq!(docs.len(), 2);
    let statuses: Vec<_> = docs.iter().filter_map(|d| d.get_str("status")).collect();
    assert!(statuses.contains(&"draft"));
    assert!(statuses.contains(&"archived"));
}

#[test]
fn filter_not_in() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::NotIn(vec!["draft".to_string(), "archived".to_string()]),
        })])
        .build();
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
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::Exists,
        })])
        .build();
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    // 4 rows have status set, 1 is NULL
    assert_eq!(docs.len(), 4);
}

#[test]
fn filter_not_exists() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "status".to_string(),
            op: query::FilterOp::NotExists,
        })])
        .build();
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    // Only "Epsilon Post" has NULL status
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("title"), Some("Epsilon Post"));
}

#[test]
fn filter_or_clause() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Or(vec![
            vec![query::Filter {
                field: "title".to_string(),
                op: query::FilterOp::Contains("Alpha".to_string()),
            }],
            vec![query::Filter {
                field: "title".to_string(),
                op: query::FilterOp::Contains("Gamma".to_string()),
            }],
        ])])
        .build();
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
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Or(vec![
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
        ])])
        .build();
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
    let q = query::FindQuery::builder()
        .filters(vec![
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
        ])
        .build();
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    // Alpha is published, Beta is draft → only Alpha matches
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("title"), Some("Alpha Post"));
}

#[test]
fn select_fields_in_find() {
    let (_tmp, pool, def) = seed_posts();
    let q = query::FindQuery::builder()
        .select(Some(vec!["title".to_string()]))
        .build();
    let docs = ops::find_documents(&pool, "posts", &def, &q, None).expect("Find failed");
    assert!(!docs.is_empty());
    for doc in &docs {
        // id is always included
        assert!(!doc.id.is_empty());
        // title should be present
        assert!(
            doc.fields.contains_key("title"),
            "title should be in fields"
        );
        // status should NOT be present (not selected)
        assert!(
            !doc.fields.contains_key("status"),
            "status should not be in fields"
        );
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
    let q = query::FindQuery::builder()
        .filters(vec![
            query::FilterClause::Single(query::Filter {
                field: "status".to_string(),
                op: query::FilterOp::Equals("published".to_string()),
            }),
            query::FilterClause::Single(query::Filter {
                field: "title".to_string(),
                op: query::FilterOp::Contains("Alpha".to_string()),
            }),
        ])
        .build();
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
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::Contains("50%".to_string()),
        })])
        .build();
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
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::Contains("a_b".to_string()),
        })])
        .build();
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
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::Equals("test".to_string()),
        })])
        .order_by(Some("status".to_string()))
        .build();
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
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "nonexistent_field".to_string(),
            op: query::FilterOp::Equals("test".to_string()),
        })])
        .build();
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
    let q = query::FindQuery::builder()
        .order_by(Some("nonexistent_field".to_string()))
        .build();
    let result = query::validate_query_fields(&def, &q, None);
    assert!(result.is_err(), "Invalid order_by field should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("nonexistent_field"),
        "Error should mention the invalid field name, got: {}",
        err_msg
    );
}

// ── Dot-notation / sub-field filter integration tests ───────────────────────

/// Build a collection with array, blocks, group, and has-many relationship fields
/// for testing dot-notation sub-field filtering.
fn make_filterable_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("products");
    def.timestamps = true;
    def.fields = vec![
        make_field("name", FieldType::Text),
        // Group field
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                make_field("meta_title", FieldType::Text),
                make_field("meta_description", FieldType::Text),
            ])
            .build(),
        // Array field with sub-fields (including a Group sub-field)
        FieldDefinition::builder("variants", FieldType::Array)
            .fields(vec![
                make_field("sku", FieldType::Text),
                make_field("color", FieldType::Text),
                make_field("size", FieldType::Text),
                FieldDefinition::builder("dimensions", FieldType::Group)
                    .fields(vec![
                        make_field("width", FieldType::Text),
                        make_field("height", FieldType::Text),
                    ])
                    .build(),
            ])
            .build(),
        // Blocks field
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![
                BlockDefinition::new("text", vec![make_field("body", FieldType::Textarea)]),
                BlockDefinition::new(
                    "image",
                    vec![
                        make_field("url", FieldType::Text),
                        make_field("alt", FieldType::Text),
                    ],
                ),
                BlockDefinition::new(
                    "section",
                    vec![
                        make_field("heading", FieldType::Text),
                        FieldDefinition::builder("meta", FieldType::Group)
                            .fields(vec![make_field("author", FieldType::Text)])
                            .build(),
                    ],
                ),
            ])
            .build(),
        // Has-many relationship
        FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("product_tags", true))
            .build(),
    ];
    def
}

fn setup_filterable() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    CollectionDefinition,
) {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_filterable_def();
    let mut tags_def = CollectionDefinition::new("product_tags");
    tags_def.timestamps = true;
    tags_def.fields = vec![make_field("label", FieldType::Text)];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
        reg.register_collection(tags_def);
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");
    (_tmp, pool, def)
}

/// Seed two products with different array/block/relationship data.
fn seed_filterable_products(
    pool: &crap_cms::db::DbPool,
    def: &CollectionDefinition,
) -> (String, String) {
    let variants_field = def.fields.iter().find(|f| f.name == "variants").unwrap();

    // Product 1: "Widget" with red variant, text block, tagged "sale"
    let mut conn = pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let mut data1 = HashMap::new();
    data1.insert("name".to_string(), "Widget".to_string());
    data1.insert("seo__meta_title".to_string(), "Buy Widget".to_string());
    data1.insert(
        "seo__meta_description".to_string(),
        "Best widget".to_string(),
    );
    let doc1 = query::create(&tx, "products", def, &data1, None).unwrap();
    let id1 = doc1.id.to_string();

    // Array rows for product 1
    let rows1 = vec![HashMap::from([
        ("sku".to_string(), "W-001".to_string()),
        ("color".to_string(), "red".to_string()),
        ("size".to_string(), "large".to_string()),
        (
            "dimensions".to_string(),
            r#"{"width":"10","height":"20"}"#.to_string(),
        ),
    ])];
    query::set_array_rows(
        &tx,
        "products",
        "variants",
        &id1,
        &rows1,
        &variants_field.fields,
        None,
    )
    .unwrap();

    // Block rows for product 1
    let blocks1 = vec![
        json!({"_block_type": "text", "body": "Widget description here"}),
        json!({"_block_type": "image", "url": "/widget.png", "alt": "Widget photo"}),
    ];
    query::set_block_rows(&tx, "products", "content", &id1, &blocks1, None).unwrap();

    // Relationship for product 1
    query::set_related_ids(
        &tx,
        "products",
        "tags",
        &id1,
        &["tag-sale".to_string()],
        None,
    )
    .unwrap();

    tx.commit().unwrap();

    // Product 2: "Gadget" with blue variant, section block, tagged "new"
    let mut conn2 = pool.get().unwrap();
    let tx2 = conn2.transaction().unwrap();
    let mut data2 = HashMap::new();
    data2.insert("name".to_string(), "Gadget".to_string());
    data2.insert("seo__meta_title".to_string(), "Buy Gadget".to_string());
    data2.insert(
        "seo__meta_description".to_string(),
        "Cool gadget".to_string(),
    );
    let doc2 = query::create(&tx2, "products", def, &data2, None).unwrap();
    let id2 = doc2.id.to_string();

    let rows2 = vec![HashMap::from([
        ("sku".to_string(), "G-001".to_string()),
        ("color".to_string(), "blue".to_string()),
        ("size".to_string(), "small".to_string()),
        (
            "dimensions".to_string(),
            r#"{"width":"5","height":"15"}"#.to_string(),
        ),
    ])];
    query::set_array_rows(
        &tx2,
        "products",
        "variants",
        &id2,
        &rows2,
        &variants_field.fields,
        None,
    )
    .unwrap();

    let blocks2 = vec![
        json!({"_block_type": "section", "heading": "About Gadget", "meta": {"author": "Alice"}}),
    ];
    query::set_block_rows(&tx2, "products", "content", &id2, &blocks2, None).unwrap();

    query::set_related_ids(
        &tx2,
        "products",
        "tags",
        &id2,
        &["tag-new".to_string()],
        None,
    )
    .unwrap();

    tx2.commit().unwrap();

    (id1, id2)
}

#[test]
fn filter_array_subfield() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by array sub-field: variants.color = "red"
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "variants.color".to_string(),
            op: query::FilterOp::Equals("red".to_string()),
        })])
        .build();
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Widget"));
}

#[test]
fn filter_array_subfield_contains() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by array sub-field: variants.sku contains "G-"
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "variants.sku".to_string(),
            op: query::FilterOp::Contains("G-".to_string()),
        })])
        .build();
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Gadget"));
}

#[test]
fn filter_array_subfield_size() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by array sub-field: variants.size = "large"
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "variants.size".to_string(),
            op: query::FilterOp::Equals("large".to_string()),
        })])
        .build();
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Widget"));
}

#[test]
fn filter_block_subfield() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by block sub-field: content.body contains "description"
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "content.body".to_string(),
            op: query::FilterOp::Contains("description".to_string()),
        })])
        .build();
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Widget"));
}

#[test]
fn filter_block_type() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by block type: content._block_type = "section"
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "content._block_type".to_string(),
            op: query::FilterOp::Equals("section".to_string()),
        })])
        .build();
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Gadget"));
}

#[test]
fn filter_block_group_subfield() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by group-in-block: content.meta.author = "Alice"
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "content.meta.author".to_string(),
            op: query::FilterOp::Equals("Alice".to_string()),
        })])
        .build();
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Gadget"));
}

#[test]
fn filter_has_many_relationship() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by has-many relationship: tags.id = "tag-sale"
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "tags.id".to_string(),
            op: query::FilterOp::Equals("tag-sale".to_string()),
        })])
        .build();
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

    let q = query::FindQuery::builder().filters(filters).build();
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
    let q = query::FindQuery::builder()
        .filters(vec![
            query::FilterClause::Single(query::Filter {
                field: "name".to_string(),
                op: query::FilterOp::Equals("Widget".to_string()),
            }),
            query::FilterClause::Single(query::Filter {
                field: "variants.color".to_string(),
                op: query::FilterOp::Equals("red".to_string()),
            }),
        ])
        .build();
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Widget"));

    // Non-matching combination: name = "Widget" AND variants.color = "blue" → 0 results
    let q2 = query::FindQuery::builder()
        .filters(vec![
            query::FilterClause::Single(query::Filter {
                field: "name".to_string(),
                op: query::FilterOp::Equals("Widget".to_string()),
            }),
            query::FilterClause::Single(query::Filter {
                field: "variants.color".to_string(),
                op: query::FilterOp::Equals("blue".to_string()),
            }),
        ])
        .build();
    let docs2 = ops::find_documents(&pool, "products", &def, &q2, None).unwrap();
    assert_eq!(docs2.len(), 0);
}

#[test]
fn filter_or_with_subquery() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // OR group with subquery filters:
    // variants.color = "red" OR content._block_type = "section"
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Or(vec![
            vec![query::Filter {
                field: "variants.color".to_string(),
                op: query::FilterOp::Equals("red".to_string()),
            }],
            vec![query::Filter {
                field: "content._block_type".to_string(),
                op: query::FilterOp::Equals("section".to_string()),
            }],
        ])])
        .build();
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 2); // Both products match one of the conditions
}

#[test]
fn filter_subquery_no_match() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter that matches nothing
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "variants.color".to_string(),
            op: query::FilterOp::Equals("green".to_string()),
        })])
        .build();
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
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "nonexistent.field".to_string(),
            op: query::FilterOp::Equals("x".to_string()),
        })])
        .build();
    let result = ops::find_documents(&pool, "products", &def, &q, None);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Invalid field"));
}

#[test]
fn filter_array_group_subfield() {
    let (_tmp, pool, def) = setup_filterable();
    seed_filterable_products(&pool, &def);

    // Filter by group-in-array: variants.dimensions.width = "10" (Widget)
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "variants.dimensions.width".to_string(),
            op: query::FilterOp::Equals("10".to_string()),
        })])
        .build();
    let docs = ops::find_documents(&pool, "products", &def, &q, None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("name"), Some("Widget"));

    // Filter by group-in-array: variants.dimensions.height = "15" (Gadget)
    let q2 = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "variants.dimensions.height".to_string(),
            op: query::FilterOp::Equals("15".to_string()),
        })])
        .build();
    let docs2 = ops::find_documents(&pool, "products", &def, &q2, None).unwrap();
    assert_eq!(docs2.len(), 1);
    assert_eq!(docs2[0].get_str("name"), Some("Gadget"));
}

// ── Regression: type-aware filter value binding ──────────────────────────

/// Regression for the `DbValue::Text` bug on numeric comparisons.
///
/// Before the fix, `gt`/`lt`/`gte`/`lte` on a `Number` field bound the
/// operand as `DbValue::Text(v)`, and SQLite's implicit text→number
/// coercion papered over the mismatch. This test populates a table with
/// actual numeric values and asserts the filter returns the right rows
/// when bound correctly — it stays green on SQLite but also guards the
/// contract for backends (e.g. Postgres) that don't silently coerce.
#[test]
fn numeric_greater_than_with_actual_integers_in_db() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let pool = pool::create_pool(tmp.path(), &config).expect("pool");

    let mut def = CollectionDefinition::new("scores");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("label", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("value", FieldType::Number).build(),
    ];

    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("sync");

    let rows: Vec<(&str, &str)> = vec![
        ("one", "1"),
        ("ten", "10"),
        ("hundred", "100"),
        ("thousand", "1000"),
    ];
    for (label, val) in &rows {
        let mut data = HashMap::new();
        data.insert("label".to_string(), label.to_string());
        data.insert("value".to_string(), val.to_string());
        let mut conn = pool.get().expect("conn");
        let tx = conn.transaction().expect("tx");
        query::create(&tx, "scores", &def, &data, None).expect("create");
        tx.commit().expect("commit");
    }

    // gt 50 must return "hundred" and "thousand" — not "one" or "ten".
    // If the value were bound as Text, "9" > "50" lexicographically would
    // flip this result. (Our inputs are chosen so text/numeric ordering
    // would diverge for "1000" vs "50": text "1000" < "50", numeric
    // 1000 > 50.) The fix binds as Real, which is unambiguously correct.
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "value".to_string(),
            op: query::FilterOp::GreaterThan("50".to_string()),
        })])
        .build();
    let docs = ops::find_documents(&pool, "scores", &def, &q, None).expect("find");
    let labels: Vec<_> = docs.iter().filter_map(|d| d.get_str("label")).collect();
    assert_eq!(labels.len(), 2, "expected 2 rows > 50, got {:?}", labels);
    assert!(labels.contains(&"hundred"));
    assert!(labels.contains(&"thousand"));

    // lt 50 returns "one" and "ten".
    let q2 = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "value".to_string(),
            op: query::FilterOp::LessThan("50".to_string()),
        })])
        .build();
    let docs2 = ops::find_documents(&pool, "scores", &def, &q2, None).expect("find");
    let labels2: Vec<_> = docs2.iter().filter_map(|d| d.get_str("label")).collect();
    assert_eq!(labels2.len(), 2, "expected 2 rows < 50, got {:?}", labels2);
    assert!(labels2.contains(&"one"));
    assert!(labels2.contains(&"ten"));

    // Regression: a text-shaped input that doesn't parse as a number must
    // fall back to Text comparison (and thus match 0 rows here, not error).
    let q3 = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "value".to_string(),
            op: query::FilterOp::GreaterThan("not-a-number".to_string()),
        })])
        .build();
    let docs3 = ops::find_documents(&pool, "scores", &def, &q3, None).expect("find");
    assert_eq!(docs3.len(), 0);
}

// ── Localized array sub-field filter (dot notation) ─────────────────────────

fn locale_config_en_de() -> LocaleConfig {
    LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}

fn make_localized_array_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("l10n_articles");
    def.timestamps = true;

    // `links` is a localized array — rows are stored per-locale.
    let links_field = FieldDefinition {
        name: "links".to_string(),
        field_type: FieldType::Array,
        localized: true,
        fields: vec![
            make_field("url", FieldType::Text),
            make_field("label", FieldType::Text),
        ],
        ..Default::default()
    };

    def.fields = vec![make_field("slug_field", FieldType::Text), links_field];
    def
}

/// Regression: filtering on `array_field.sub_field` while scoped to a single
/// locale must only match rows belonging to that locale. Previously the
/// EXISTS subquery did not add a `_locale = ?` constraint, so a filter in
/// the `de` locale would still match documents that only had the value in
/// their `en` rows.
#[test]
fn filter_localized_field_in_array_routes_to_locale_column() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_localized_array_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }

    let locale_config = locale_config_en_de();
    migrate::sync_all(&pool, &registry, &locale_config).expect("sync");

    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();

    // Doc A: label "Shared" only in the EN locale.
    // Doc B: label "Shared" only in the DE locale.
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    let mut data_a = HashMap::new();
    data_a.insert("slug_field".to_string(), "a".to_string());
    let doc_a = query::create(&tx, "l10n_articles", &def, &data_a, None).expect("create a");

    let mut data_b = HashMap::new();
    data_b.insert("slug_field".to_string(), "b".to_string());
    let doc_b = query::create(&tx, "l10n_articles", &def, &data_b, None).expect("create b");

    let a_en = vec![HashMap::from([
        ("url".to_string(), "https://a-en".to_string()),
        ("label".to_string(), "Shared".to_string()),
    ])];
    let a_de = vec![HashMap::from([
        ("url".to_string(), "https://a-de".to_string()),
        ("label".to_string(), "A-German-Only".to_string()),
    ])];
    let b_en = vec![HashMap::from([
        ("url".to_string(), "https://b-en".to_string()),
        ("label".to_string(), "B-English-Only".to_string()),
    ])];
    let b_de = vec![HashMap::from([
        ("url".to_string(), "https://b-de".to_string()),
        ("label".to_string(), "Shared".to_string()),
    ])];

    query::set_array_rows(
        &tx,
        "l10n_articles",
        "links",
        &doc_a.id,
        &a_en,
        &links_field.fields,
        Some("en"),
    )
    .expect("set a en");
    query::set_array_rows(
        &tx,
        "l10n_articles",
        "links",
        &doc_a.id,
        &a_de,
        &links_field.fields,
        Some("de"),
    )
    .expect("set a de");
    query::set_array_rows(
        &tx,
        "l10n_articles",
        "links",
        &doc_b.id,
        &b_en,
        &links_field.fields,
        Some("en"),
    )
    .expect("set b en");
    query::set_array_rows(
        &tx,
        "l10n_articles",
        "links",
        &doc_b.id,
        &b_de,
        &links_field.fields,
        Some("de"),
    )
    .expect("set b de");

    tx.commit().expect("commit");

    let de_ctx = LocaleContext {
        mode: LocaleMode::Single("de".into()),
        config: locale_config.clone(),
    };

    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "links.label".to_string(),
            op: query::FilterOp::Equals("Shared".to_string()),
        })])
        .build();

    let docs =
        ops::find_documents(&pool, "l10n_articles", &def, &q, Some(&de_ctx)).expect("find de");

    let ids: Vec<&str> = docs.iter().map(|d| d.id.as_ref()).collect();

    assert_eq!(
        ids.len(),
        1,
        "filtering links.label=Shared in DE must match exactly one doc (doc B), got: {ids:?}"
    );
    assert_eq!(
        ids[0],
        doc_b.id.as_ref(),
        "the matching doc must be the one whose DE rows contain 'Shared'"
    );
}

/// Seed the `l10n_articles` fixture with docs A and B each carrying distinct
/// `label` values in EN and DE rows of the localized `links` array.
///
/// Returns `(tmp_dir, pool, def, doc_a_id, doc_b_id, locale_config)` — the
/// `tmp_dir` must be kept alive for the pool to remain usable.
fn seed_l10n_articles_fixture() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    CollectionDefinition,
    String,
    String,
    LocaleConfig,
) {
    let (tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_localized_array_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }

    let locale_config = locale_config_en_de();
    migrate::sync_all(&pool, &registry, &locale_config).expect("sync");

    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    let mut data_a = HashMap::new();
    data_a.insert("slug_field".to_string(), "a".to_string());
    let doc_a = query::create(&tx, "l10n_articles", &def, &data_a, None).expect("create a");

    let mut data_b = HashMap::new();
    data_b.insert("slug_field".to_string(), "b".to_string());
    let doc_b = query::create(&tx, "l10n_articles", &def, &data_b, None).expect("create b");

    let a_en = vec![HashMap::from([
        ("url".to_string(), "https://a-en".to_string()),
        ("label".to_string(), "Shared".to_string()),
    ])];
    let a_de = vec![HashMap::from([
        ("url".to_string(), "https://a-de".to_string()),
        ("label".to_string(), "A-German-Only".to_string()),
    ])];
    let b_en = vec![HashMap::from([
        ("url".to_string(), "https://b-en".to_string()),
        ("label".to_string(), "B-English-Only".to_string()),
    ])];
    let b_de = vec![HashMap::from([
        ("url".to_string(), "https://b-de".to_string()),
        ("label".to_string(), "Shared".to_string()),
    ])];

    let seed_rows = [
        (&doc_a.id, &a_en, "en"),
        (&doc_a.id, &a_de, "de"),
        (&doc_b.id, &b_en, "en"),
        (&doc_b.id, &b_de, "de"),
    ];
    for (doc_id, rows, loc) in seed_rows {
        query::set_array_rows(
            &tx,
            "l10n_articles",
            "links",
            doc_id,
            rows,
            &links_field.fields,
            Some(loc),
        )
        .unwrap_or_else(|e| panic!("set {loc} for {doc_id}: {e}"));
    }

    tx.commit().expect("commit");

    (
        tmp,
        pool,
        def,
        doc_a.id.to_string(),
        doc_b.id.to_string(),
        locale_config,
    )
}

/// Regression: `LocaleMode::All` must NOT add a `_locale = ?` constraint —
/// the same filter should match BOTH docs (one in EN, one in DE).
#[test]
fn filter_localized_field_in_array_with_all_locale_matches_any() {
    let (_tmp, pool, def, doc_a_id, doc_b_id, locale_config) = seed_l10n_articles_fixture();

    let all_ctx = LocaleContext {
        mode: LocaleMode::All,
        config: locale_config,
    };

    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "links.label".to_string(),
            op: query::FilterOp::Equals("Shared".to_string()),
        })])
        .build();

    let docs =
        ops::find_documents(&pool, "l10n_articles", &def, &q, Some(&all_ctx)).expect("find all");

    let mut ids: Vec<&str> = docs.iter().map(|d| d.id.as_ref()).collect();
    ids.sort();

    let mut expected = vec![doc_a_id.as_str(), doc_b_id.as_str()];
    expected.sort();

    assert_eq!(
        ids, expected,
        "filtering links.label=Shared with LocaleMode::All must match both docs"
    );
}

/// Regression: `LocaleMode::Default` (no explicit locale) must use the
/// default locale (`en`) — the filter should match doc A only, whose EN
/// row has `label = 'Shared'`.
#[test]
fn filter_localized_field_in_array_with_default_locale_uses_default() {
    let (_tmp, pool, def, doc_a_id, _doc_b_id, locale_config) = seed_l10n_articles_fixture();

    let default_ctx = LocaleContext {
        mode: LocaleMode::Default,
        config: locale_config,
    };

    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "links.label".to_string(),
            op: query::FilterOp::Equals("Shared".to_string()),
        })])
        .build();

    let docs = ops::find_documents(&pool, "l10n_articles", &def, &q, Some(&default_ctx))
        .expect("find default");

    let ids: Vec<&str> = docs.iter().map(|d| d.id.as_ref()).collect();

    assert_eq!(
        ids.len(),
        1,
        "filtering links.label=Shared under LocaleMode::Default must match exactly doc A, got: {ids:?}"
    );
    assert_eq!(
        ids[0], doc_a_id,
        "the matching doc must be doc A (whose EN rows contain 'Shared')"
    );
}
