//! Validation-focused tests for crap-cms hook lifecycle.
//!
//! Tests for validate_fields: required, unique, custom functions,
//! block/array sub-fields, groups, min/max rows, date format, nested
//! structures, and related edge cases.

use std::collections::HashMap;
use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::field::{BlockDefinition, FieldDefinition, FieldTab, FieldType};
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::{HookRunner, ValidationCtx};
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
) -> crap_cms::core::Document {
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

fn make_field(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, field_type).build()
}

// ── 2B. Validate Fields ──────────────────────────────────────────────────────

#[test]
fn validate_required_present_passes() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), json!("Valid Title"));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &def.fields,
        &data,
        &ValidationCtx::builder(&conn, "articles").build(),
    );
    assert!(
        result.is_ok(),
        "Validation should pass with required field present"
    );
}

#[test]
fn validate_required_missing_fails() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let data = HashMap::new(); // title is missing

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &def.fields,
        &data,
        &ValidationCtx::builder(&conn, "articles").build(),
    );
    assert!(
        result.is_err(),
        "Validation should fail with missing required field"
    );
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field == "title"),
        "Should have title error"
    );
}

#[test]
fn validate_required_empty_string_fails() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), json!("")); // empty string

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &def.fields,
        &data,
        &ValidationCtx::builder(&conn, "articles").build(),
    );
    assert!(
        result.is_err(),
        "Validation should fail with empty required field"
    );
}

#[test]
fn validate_unique_passes_when_no_conflict() {
    let (_tmp, pool, registry, runner) = setup();

    // Create one article
    let mut create_data = HashMap::new();
    create_data.insert("title".to_string(), "Unique Title".to_string());
    let _doc = create_article(&pool, &registry, &create_data);

    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Validate a different title — should pass
    let mut data = HashMap::new();
    data.insert("title".to_string(), json!("Different Title"));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &def.fields,
        &data,
        &ValidationCtx::builder(&conn, "articles").build(),
    );
    assert!(
        result.is_ok(),
        "Unique validation should pass with different title"
    );
}

#[test]
fn validate_unique_fails_on_duplicate() {
    let (_tmp, pool, registry, runner) = setup();

    // Create one article
    let mut create_data = HashMap::new();
    create_data.insert("title".to_string(), "Duplicate Title".to_string());
    let _doc = create_article(&pool, &registry, &create_data);

    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Validate same title — should fail
    let mut data = HashMap::new();
    data.insert("title".to_string(), json!("Duplicate Title"));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &def.fields,
        &data,
        &ValidationCtx::builder(&conn, "articles").build(),
    );
    assert!(
        result.is_err(),
        "Unique validation should fail for duplicate title"
    );
    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.field == "title" && e.message.contains("unique"))
    );
}

#[test]
fn validate_unique_excludes_self_on_update() {
    let (_tmp, pool, registry, runner) = setup();

    // Create one article
    let mut create_data = HashMap::new();
    create_data.insert("title".to_string(), "My Title".to_string());
    let doc = create_article(&pool, &registry, &create_data);

    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    // Validate same title with exclude_id = self — should pass (updating own doc)
    let mut data = HashMap::new();
    data.insert("title".to_string(), json!("My Title"));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &def.fields,
        &data,
        &ValidationCtx::builder(&conn, "articles")
            .exclude_id(Some(&doc.id))
            .build(),
    );
    assert!(
        result.is_ok(),
        "Unique validation should pass when excluding own ID"
    );
}

// ── 2C. Custom Validate Functions ────────────────────────────────────────────

#[test]
fn custom_validate_function_passes() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), json!("Valid"));
    data.insert("word_count".to_string(), json!(42)); // positive

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &def.fields,
        &data,
        &ValidationCtx::builder(&conn, "articles").build(),
    );
    assert!(
        result.is_ok(),
        "Custom validate should pass for positive number"
    );
}

#[test]
fn custom_validate_function_fails() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), json!("Valid"));
    data.insert("word_count".to_string(), json!(-5)); // negative!

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &def.fields,
        &data,
        &ValidationCtx::builder(&conn, "articles").build(),
    );
    assert!(
        result.is_err(),
        "Custom validate should fail for negative number"
    );
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field == "word_count"),
        "Should have word_count error"
    );
}

#[test]
fn custom_validate_returns_error_message() {
    let (_tmp, pool, registry, runner) = setup();
    let reg = registry.read().unwrap();
    let def = reg.get_collection("articles").unwrap().clone();
    drop(reg);

    let mut data = HashMap::new();
    data.insert("title".to_string(), json!("Valid"));
    data.insert("word_count".to_string(), json!(-1));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &def.fields,
        &data,
        &ValidationCtx::builder(&conn, "articles").build(),
    );
    let err = result.unwrap_err();
    let word_count_err = err.errors.iter().find(|e| e.field == "word_count").unwrap();
    assert!(
        word_count_err.message.contains("positive"),
        "Error message should mention 'positive', got: {}",
        word_count_err.message
    );
}

// ── 2D. Block/Array Sub-field Validation ─────────────────────────────────────

#[test]
fn validate_blocks_required_subfield_fails_when_empty() {
    let (_tmp, pool, _registry, runner) = setup();

    // Build a blocks field definition with a required sub-field
    let blocks_field = FieldDefinition::builder("content", FieldType::Blocks)
        .blocks(vec![crap_cms::core::field::BlockDefinition::new(
            "text",
            vec![
                FieldDefinition::builder("title", FieldType::Text)
                    .required(true)
                    .build(),
                FieldDefinition::builder("body", FieldType::Textarea).build(),
            ],
        )])
        .build();

    let fields = vec![blocks_field];
    let mut data = HashMap::new();
    data.insert(
        "content".to_string(),
        json!([
            { "_block_type": "text", "title": "", "body": "some content" }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "articles").build(),
    );
    assert!(
        result.is_err(),
        "Validation should fail with empty required block sub-field"
    );
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field == "content[0][title]"),
        "Should have content[0][title] error, got: {:?}",
        err.errors.iter().map(|e| &e.field).collect::<Vec<_>>()
    );
}

#[test]
fn validate_blocks_required_subfield_passes_when_present() {
    let (_tmp, pool, _registry, runner) = setup();

    let blocks_field = FieldDefinition::builder("content", FieldType::Blocks)
        .blocks(vec![crap_cms::core::field::BlockDefinition::new(
            "text",
            vec![
                FieldDefinition::builder("title", FieldType::Text)
                    .required(true)
                    .build(),
            ],
        )])
        .build();

    let fields = vec![blocks_field];
    let mut data = HashMap::new();
    data.insert(
        "content".to_string(),
        json!([
            { "_block_type": "text", "title": "Hello World" }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "articles").build(),
    );
    assert!(
        result.is_ok(),
        "Validation should pass when required block sub-field is present"
    );
}

#[test]
fn validate_blocks_skips_required_for_drafts() {
    let (_tmp, pool, _registry, runner) = setup();

    let blocks_field = FieldDefinition::builder("content", FieldType::Blocks)
        .blocks(vec![crap_cms::core::field::BlockDefinition::new(
            "text",
            vec![
                FieldDefinition::builder("title", FieldType::Text)
                    .required(true)
                    .build(),
            ],
        )])
        .build();

    let fields = vec![blocks_field];
    let mut data = HashMap::new();
    data.insert(
        "content".to_string(),
        json!([
            { "_block_type": "text", "title": "" }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "articles")
            .draft(true)
            .build(),
    );
    assert!(
        result.is_ok(),
        "Validation should skip required sub-fields for drafts"
    );
}

#[test]
fn validate_array_required_subfield_fails_when_empty() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition::builder("items", FieldType::Array)
        .fields(vec![
            FieldDefinition::builder("label", FieldType::Text)
                .required(true)
                .build(),
        ])
        .build();

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert(
        "items".to_string(),
        json!([
            { "label": "ok" },
            { "label": "" }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "articles").build(),
    );
    assert!(
        result.is_err(),
        "Validation should fail for empty required array sub-field"
    );
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field == "items[1][label]"),
        "Should have items[1][label] error, got: {:?}",
        err.errors.iter().map(|e| &e.field).collect::<Vec<_>>()
    );
    // First row should pass
    assert!(
        !err.errors.iter().any(|e| e.field == "items[0][label]"),
        "First row should not have error"
    );
}

// ── 6D. Validate: Group Sub-fields ───────────────────────────────────────────

#[test]
fn validate_group_required_subfield_fails() {
    let (_tmp, pool, _registry, runner) = setup();

    let group_field = FieldDefinition::builder("seo", FieldType::Group)
        .fields(vec![
            FieldDefinition::builder("meta_title", FieldType::Text)
                .required(true)
                .build(),
        ])
        .build();

    let fields = vec![group_field];
    let mut data = HashMap::new();
    // Group sub-fields are stored as group__subfield at top level
    data.insert("seo__meta_title".to_string(), json!(""));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test_table").build(),
    );
    assert!(
        result.is_err(),
        "Validation should fail for empty required group sub-field"
    );
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field == "seo__meta_title"),
        "Should have seo__meta_title error, got: {:?}",
        err.errors
    );
}

#[test]
fn validate_group_required_subfield_passes() {
    let (_tmp, pool, _registry, runner) = setup();

    let group_field = FieldDefinition::builder("seo", FieldType::Group)
        .fields(vec![
            FieldDefinition::builder("meta_title", FieldType::Text)
                .required(true)
                .build(),
        ])
        .build();

    let fields = vec![group_field];
    let mut data = HashMap::new();
    data.insert("seo__meta_title".to_string(), json!("My Title"));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test_table").build(),
    );
    assert!(
        result.is_ok(),
        "Validation should pass for non-empty required group sub-field"
    );
}

#[test]
fn validate_group_skips_for_drafts() {
    let (_tmp, pool, _registry, runner) = setup();

    let group_field = FieldDefinition::builder("seo", FieldType::Group)
        .fields(vec![
            FieldDefinition::builder("meta_title", FieldType::Text)
                .required(true)
                .build(),
        ])
        .build();

    let fields = vec![group_field];
    let mut data = HashMap::new();
    data.insert("seo__meta_title".to_string(), json!(""));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test_table")
            .draft(true)
            .build(),
    );
    assert!(
        result.is_ok(),
        "Validation should skip group sub-field required check for drafts"
    );
}

// ── 6E. Validate: min_rows / max_rows ────────────────────────────────────────

#[test]
fn validate_min_rows_fails() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition::builder("items", FieldType::Array)
        .min_rows(2)
        .fields(vec![
            FieldDefinition::builder("label", FieldType::Text).build(),
        ])
        .build();

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert(
        "items".to_string(),
        json!([
            { "label": "only one" }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test_table").build(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.field == "items" && e.message.contains("at least 2"))
    );
}

#[test]
fn validate_max_rows_fails() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition::builder("items", FieldType::Array)
        .max_rows(1)
        .fields(vec![
            FieldDefinition::builder("label", FieldType::Text).build(),
        ])
        .build();

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert(
        "items".to_string(),
        json!([
            { "label": "one" },
            { "label": "two" }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test_table").build(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.field == "items" && e.message.contains("at most 1"))
    );
}

#[test]
fn validate_min_max_rows_passes_when_in_range() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition::builder("items", FieldType::Array)
        .min_rows(1)
        .max_rows(3)
        .fields(vec![
            FieldDefinition::builder("label", FieldType::Text).build(),
        ])
        .build();

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert(
        "items".to_string(),
        json!([
            { "label": "one" },
            { "label": "two" }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test_table").build(),
    );
    assert!(result.is_ok());
}

#[test]
fn validate_min_rows_skips_for_drafts() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition::builder("items", FieldType::Array)
        .min_rows(5)
        .fields(vec![
            FieldDefinition::builder("label", FieldType::Text).build(),
        ])
        .build();

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert("items".to_string(), json!([]));

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test_table")
            .draft(true)
            .build(),
    );
    assert!(result.is_ok(), "min_rows should be skipped for drafts");
}

// ── 6F. Validate: Date Format ────────────────────────────────────────────────

#[test]
fn validate_date_field_valid_formats() {
    let (_tmp, pool, _registry, runner) = setup();

    let date_field = FieldDefinition::builder("due_date", FieldType::Date).build();

    let fields = vec![date_field];
    let conn = pool.get().expect("DB connection");

    // YYYY-MM-DD
    let mut data = HashMap::new();
    data.insert("due_date".to_string(), json!("2024-01-15"));
    assert!(
        runner
            .validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build())
            .is_ok()
    );

    // YYYY-MM-DDTHH:MM
    data.insert("due_date".to_string(), json!("2024-01-15T14:30"));
    assert!(
        runner
            .validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build())
            .is_ok()
    );

    // YYYY-MM-DDTHH:MM:SS
    data.insert("due_date".to_string(), json!("2024-01-15T14:30:00"));
    assert!(
        runner
            .validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build())
            .is_ok()
    );

    // Full ISO 8601
    data.insert("due_date".to_string(), json!("2024-01-15T14:30:00+00:00"));
    assert!(
        runner
            .validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build())
            .is_ok()
    );

    // Time only: HH:MM
    data.insert("due_date".to_string(), json!("14:30"));
    assert!(
        runner
            .validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build())
            .is_ok()
    );

    // Time only: HH:MM:SS
    data.insert("due_date".to_string(), json!("14:30:00"));
    assert!(
        runner
            .validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build())
            .is_ok()
    );

    // Month only: YYYY-MM
    data.insert("due_date".to_string(), json!("2024-01"));
    assert!(
        runner
            .validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build())
            .is_ok()
    );
}

#[test]
fn validate_date_field_invalid_format() {
    let (_tmp, pool, _registry, runner) = setup();

    let date_field = FieldDefinition::builder("due_date", FieldType::Date).build();

    let fields = vec![date_field];
    let conn = pool.get().expect("DB connection");

    let mut data = HashMap::new();
    data.insert("due_date".to_string(), json!("not-a-date"));
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.field == "due_date" && e.message.contains("valid date"))
    );
}

#[test]
fn validate_date_empty_is_ok() {
    let (_tmp, pool, _registry, runner) = setup();

    let date_field = FieldDefinition::builder("due_date", FieldType::Date).build();

    let fields = vec![date_field];
    let conn = pool.get().expect("DB connection");

    // Empty string should not trigger date format validation (is_empty = true)
    let mut data = HashMap::new();
    data.insert("due_date".to_string(), json!(""));
    assert!(
        runner
            .validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build())
            .is_ok()
    );

    // Null should not trigger date format validation
    data.insert("due_date".to_string(), serde_json::Value::Null);
    assert!(
        runner
            .validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build())
            .is_ok()
    );
}

// ── 6G. Validate: Sub-field Date and Validate in Array Rows ──────────────────

#[test]
fn validate_date_subfield_in_array_rows() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition::builder("events", FieldType::Array)
        .fields(vec![
            FieldDefinition::builder("event_date", FieldType::Date).build(),
        ])
        .build();

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert(
        "events".to_string(),
        json!([
            { "event_date": "2024-01-15" },
            { "event_date": "bad-date" }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.field == "events[1][event_date]" && e.message.contains("valid date")),
        "Should have date validation error for events[1][event_date], got: {:?}",
        err.errors
    );
}

#[test]
fn validate_custom_function_in_array_subfield() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition::builder("scores", FieldType::Array)
        .fields(vec![
            FieldDefinition::builder("value", FieldType::Number)
                .validate("hooks.validators.positive_number")
                .build(),
        ])
        .build();

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert(
        "scores".to_string(),
        json!([
            { "value": 10 },
            { "value": -5 }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors.iter().any(|e| e.field == "scores[1][value]"),
        "Should have custom validate error for scores[1][value], got: {:?}",
        err.errors
    );
}

// ── 6H. Validate: Nested Array/Blocks within Array Rows ──────────────────────

#[test]
fn validate_nested_array_in_array_rows() {
    let (_tmp, pool, _registry, runner) = setup();

    let nested_array = FieldDefinition::builder("outer", FieldType::Array)
        .fields(vec![
            FieldDefinition::builder("inner", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("value", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        ])
        .build();

    let fields = vec![nested_array];
    let mut data = HashMap::new();
    data.insert(
        "outer".to_string(),
        json!([
            {
                "inner": [
                    { "value": "ok" },
                    { "value": "" }
                ]
            }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.field.contains("outer[0][inner][1][value]")),
        "Should have nested array validation error, got: {:?}",
        err.errors
    );
}

#[test]
fn validate_nested_blocks_in_array_rows() {
    let (_tmp, pool, _registry, runner) = setup();

    let outer = FieldDefinition::builder("sections", FieldType::Array)
        .fields(vec![
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "text",
                    vec![
                        FieldDefinition::builder("body", FieldType::Text)
                            .required(true)
                            .build(),
                    ],
                )])
                .build(),
        ])
        .build();

    let fields = vec![outer];
    let mut data = HashMap::new();
    data.insert(
        "sections".to_string(),
        json!([
            {
                "content": [
                    { "_block_type": "text", "body": "" }
                ]
            }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.field.contains("sections[0][content][0][body]")),
        "Should have nested blocks validation error, got: {:?}",
        err.errors
    );
}

// ── 6I. Validate: Group Sub-fields in Array Rows ─────────────────────────────

#[test]
fn validate_group_in_array_row_required_subfield() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition::builder("items", FieldType::Array)
        .fields(vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("author", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        ])
        .build();

    let fields = vec![array_field];
    let mut data = HashMap::new();
    // Group sub-fields in array rows use nested object format
    data.insert(
        "items".to_string(),
        json!([
            { "meta": { "author": "" } }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.field.contains("items[0][meta][0][author]")),
        "Should have group-in-array validation error, got: {:?}",
        err.errors
    );
}

#[test]
fn validate_group_date_subfield_in_array_row() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition::builder("items", FieldType::Array)
        .fields(vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("published_at", FieldType::Date).build(),
                ])
                .build(),
        ])
        .build();

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert(
        "items".to_string(),
        json!([
            { "meta": { "published_at": "bad-date" } }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.field.contains("items[0][meta][0][published_at]")
                && e.message.contains("valid date")),
        "Should have group-date validation error in array row, got: {:?}",
        err.errors
    );
}

#[test]
fn validate_group_custom_validate_in_array_row() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition::builder("items", FieldType::Array)
        .fields(vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("score", FieldType::Number)
                        .validate("hooks.validators.positive_number")
                        .build(),
                ])
                .build(),
        ])
        .build();

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert(
        "items".to_string(),
        json!([
            { "meta": { "score": -10 } }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.field.contains("items[0][meta][0][score]")),
        "Should have custom validate error for group sub-field in array row, got: {:?}",
        err.errors
    );
}

// ── 6J. Validate: Required field on update skips when absent ─────────────────

#[test]
fn validate_required_field_on_update_skips_when_absent() {
    let (_tmp, pool, _registry, runner) = setup();

    let fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
    ];

    // On update (exclude_id set), if the required field is absent from data
    // (partial update), it should pass — we're keeping the existing value
    let mut data = HashMap::new();
    data.insert("body".to_string(), json!("updated body only"));
    // title is NOT in data

    let conn = pool.get().expect("DB connection");
    let result = runner.validate_fields(
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "t")
            .exclude_id(Some("existing-id"))
            .build(),
    );
    assert!(
        result.is_ok(),
        "Required field should be skipped on update when absent from data"
    );
}

#[test]
fn validate_required_field_checkbox_always_passes() {
    let (_tmp, pool, _registry, runner) = setup();

    let fields = vec![
        FieldDefinition::builder("active", FieldType::Checkbox)
            .required(true)
            .build(),
    ];

    // Checkbox with required=true should pass even with no data
    let data = HashMap::new();
    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(
        result.is_ok(),
        "Checkbox field should never fail required check"
    );
}

// ── 6X. Custom validate function returning false (no message) ────────────────

#[test]
fn custom_validate_returns_false_generic_message() {
    let (_tmp, pool, _registry, runner) = setup();

    // Create a field with a validate function that returns false (not a string)
    let fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .validate("hooks.access.deny_all") // returns false
            .build(),
    ];

    let mut data = HashMap::new();
    data.insert("title".to_string(), json!("any value"));

    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.field == "title" && e.message.contains("validation failed")),
        "False-returning validate should produce 'validation failed' message, got: {:?}",
        err.errors
    );
}

// ── 6Y. Required check on Array/Relationship (has-many) fields ───────────────

#[test]
fn validate_required_array_must_have_items() {
    let (_tmp, pool, _registry, runner) = setup();

    let fields = vec![
        FieldDefinition::builder("tags", FieldType::Array)
            .required(true)
            .fields(vec![make_field("label", FieldType::Text)])
            .build(),
    ];

    let conn = pool.get().expect("DB connection");

    // Empty array should fail
    let mut data = HashMap::new();
    data.insert("tags".to_string(), json!([]));
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(result.is_err(), "Empty required array should fail");

    // Non-empty array should pass
    data.insert("tags".to_string(), json!([{"label": "rust"}]));
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(result.is_ok(), "Non-empty required array should pass");
}

// ── 6AB. Blocks: unknown block_type is rejected ─────────────────────────────

/// Regression: blocks with an unrecognized `_block_type` must be rejected so
/// that arbitrary data cannot bypass field validation.
#[test]
fn validate_blocks_unknown_block_type_rejected() {
    let (_tmp, pool, _registry, runner) = setup();

    let blocks_field = FieldDefinition::builder("content", FieldType::Blocks)
        .blocks(vec![BlockDefinition::new(
            "text",
            vec![
                FieldDefinition::builder("body", FieldType::Text)
                    .required(true)
                    .build(),
            ],
        )])
        .build();

    let fields = vec![blocks_field];
    let mut data = HashMap::new();
    data.insert(
        "content".to_string(),
        json!([
            { "_block_type": "unknown_type", "body": "" }
        ]),
    );

    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(result.is_err(), "Unknown block type must be rejected");
    let err = result.unwrap_err();
    assert!(
        err.errors[0].message.contains("unknown block type"),
        "error should mention unknown block type: {}",
        err.errors[0].message,
    );
}

// ── 6AC. Checkbox sub-field in array row never fails required ────────────────

#[test]
fn validate_checkbox_subfield_in_array_never_fails_required() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition::builder("items", FieldType::Array)
        .fields(vec![
            FieldDefinition::builder("active", FieldType::Checkbox)
                .required(true)
                .build(),
        ])
        .build();

    let fields = vec![array_field];
    let mut data = HashMap::new();
    // Checkbox absent/empty should NOT fail required check
    data.insert("items".to_string(), json!([{}]));

    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(
        result.is_ok(),
        "Checkbox subfield should never fail required check"
    );
}

// ── 6AD. Checkbox sub-field in group in array row ────────────────────────────

#[test]
fn validate_checkbox_group_subfield_in_array_never_fails_required() {
    let (_tmp, pool, _registry, runner) = setup();

    let array_field = FieldDefinition::builder("items", FieldType::Array)
        .fields(vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("featured", FieldType::Checkbox)
                        .required(true)
                        .build(),
                ])
                .build(),
        ])
        .build();

    let fields = vec![array_field];
    let mut data = HashMap::new();
    data.insert("items".to_string(), json!([{}]));

    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(
        result.is_ok(),
        "Checkbox in group in array should never fail required"
    );
}

// ── 6AE. Validate: Blocks row that is not an object ──────────────────────────

/// Regression: non-object rows in a blocks field must be rejected so that
/// primitives cannot bypass sub-field validation.
#[test]
fn validate_blocks_non_object_row_rejected() {
    let (_tmp, pool, _registry, runner) = setup();

    let blocks_field = FieldDefinition::builder("content", FieldType::Blocks)
        .blocks(vec![BlockDefinition::new(
            "text",
            vec![
                FieldDefinition::builder("body", FieldType::Text)
                    .required(true)
                    .build(),
            ],
        )])
        .build();

    let fields = vec![blocks_field];
    let mut data = HashMap::new();
    data.insert("content".to_string(), json!(["not-an-object"]));

    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(result.is_err(), "Non-object block rows must be rejected");
    let err = result.unwrap_err();
    assert!(
        err.errors[0].message.contains("must be an object"),
        "error should mention object requirement: {}",
        err.errors[0].message,
    );
}

// ── Deep nesting validation (Array → container → container → leaf) ──────────

#[test]
fn validate_row_inside_tabs_inside_array_via_hook_runner() {
    let (_tmp, pool, _registry, runner) = setup();

    // Array > Tabs > Row > required text (team_members pattern)
    let fields = vec![
        FieldDefinition::builder("team_members", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("tabs", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "Personal",
                        vec![
                            FieldDefinition::builder("name_row", FieldType::Row)
                                .fields(vec![
                                    FieldDefinition::builder("first_name", FieldType::Text)
                                        .required(true)
                                        .build(),
                                    FieldDefinition::builder("last_name", FieldType::Text)
                                        .required(true)
                                        .build(),
                                ])
                                .build(),
                        ],
                    )])
                    .build(),
            ])
            .build(),
    ];

    let mut data = HashMap::new();
    data.insert(
        "team_members".to_string(),
        json!([{"first_name": "", "last_name": ""}]),
    );

    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(
        result.is_err(),
        "Required field inside Row inside Tabs inside Array must be rejected by HookRunner"
    );
    let err = result.unwrap_err();
    assert_eq!(err.errors.len(), 2);
    assert!(err.errors.iter().any(|e| e.field.contains("first_name")));
    assert!(err.errors.iter().any(|e| e.field.contains("last_name")));
}

#[test]
fn validate_group_inside_tabs_inside_array_via_hook_runner() {
    let (_tmp, pool, _registry, runner) = setup();

    // Array > Tabs > Group > required text
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("tabs", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "SEO",
                        vec![
                            FieldDefinition::builder("meta", FieldType::Group)
                                .fields(vec![
                                    FieldDefinition::builder("title", FieldType::Text)
                                        .required(true)
                                        .build(),
                                ])
                                .build(),
                        ],
                    )])
                    .build(),
            ])
            .build(),
    ];

    let mut data = HashMap::new();
    data.insert("items".to_string(), json!([{"meta": {"title": ""}}]));

    let conn = pool.get().expect("DB connection");
    let result =
        runner.validate_fields(&fields, &data, &ValidationCtx::builder(&conn, "t").build());
    assert!(
        result.is_err(),
        "Required field inside Group inside Tabs inside Array must be rejected by HookRunner"
    );
    assert!(
        result.unwrap_err().errors[0]
            .field
            .contains("meta][0][title")
    );
}
