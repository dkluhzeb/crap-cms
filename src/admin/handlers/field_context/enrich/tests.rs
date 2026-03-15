use super::super::builder::build_field_contexts;
use super::*;
use crate::{
    admin::handlers::field_context::MAX_FIELD_DEPTH,
    core::field::{BlockDefinition, FieldDefinition, LocalizedString, SelectOption},
};

fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, ft).build()
}

// --- Recursive enrichment tests (build_enriched_sub_field_context) ---

#[test]
fn enriched_sub_field_nested_array_populates_rows() {
    let mut inner_array = make_field("images", FieldType::Array);
    inner_array.fields = vec![
        make_field("url", FieldType::Text),
        make_field("alt", FieldType::Text),
    ];

    // Simulate hydrated data: an array with 2 rows
    let raw_value = serde_json::json!([
        {"url": "img1.jpg", "alt": "First"},
        {"url": "img2.jpg", "alt": "Second"},
    ]);

    let ctx = build_enriched_sub_field_context(
        &inner_array,
        Some(&raw_value),
        "content",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );

    assert_eq!(ctx["field_type"], "array");
    assert_eq!(ctx["row_count"], 2);

    let rows = ctx["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);

    // First row sub_fields
    let row0_fields = rows[0]["sub_fields"].as_array().unwrap();
    assert_eq!(row0_fields[0]["name"], "content[0][images][0][url]");
    assert_eq!(row0_fields[0]["value"], "img1.jpg");
    assert_eq!(row0_fields[1]["name"], "content[0][images][0][alt]");
    assert_eq!(row0_fields[1]["value"], "First");

    // Second row sub_fields
    let row1_fields = rows[1]["sub_fields"].as_array().unwrap();
    assert_eq!(row1_fields[0]["value"], "img2.jpg");
    assert_eq!(row1_fields[1]["value"], "Second");

    // Template sub_fields should use __INDEX__
    let template_sub = ctx["sub_fields"].as_array().unwrap();
    assert_eq!(
        template_sub[0]["name"],
        "content[0][images][__INDEX__][url]"
    );
}

#[test]
fn enriched_sub_field_nested_blocks_populates_rows() {
    let mut inner_blocks = make_field("sections", FieldType::Blocks);
    inner_blocks.blocks = vec![{
        let mut bd = BlockDefinition::new("text", vec![make_field("body", FieldType::Richtext)]);
        bd.label = Some(LocalizedString::Plain("Text".to_string()));
        bd
    }];

    let raw_value = serde_json::json!([
        {"_block_type": "text", "body": "<p>Hello</p>"},
    ]);

    let ctx = build_enriched_sub_field_context(
        &inner_blocks,
        Some(&raw_value),
        "page",
        2,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );

    assert_eq!(ctx["field_type"], "blocks");
    assert_eq!(ctx["row_count"], 1);

    let rows = ctx["rows"].as_array().unwrap();
    assert_eq!(rows[0]["_block_type"], "text");
    assert_eq!(rows[0]["block_label"], "Text");

    let sub_fields = rows[0]["sub_fields"].as_array().unwrap();
    assert_eq!(sub_fields[0]["name"], "page[2][sections][0][body]");
    assert_eq!(sub_fields[0]["value"], "<p>Hello</p>");

    // Block definitions for templates
    let block_defs = ctx["block_definitions"].as_array().unwrap();
    assert_eq!(block_defs.len(), 1);
}

#[test]
fn enriched_sub_field_nested_group_populates_values() {
    let mut inner_group = make_field("meta", FieldType::Group);
    inner_group.fields = vec![
        make_field("author", FieldType::Text),
        make_field("published", FieldType::Checkbox),
    ];

    let raw_value = serde_json::json!({
        "author": "Alice",
        "published": "1",
    });

    let ctx = build_enriched_sub_field_context(
        &inner_group,
        Some(&raw_value),
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );

    assert_eq!(ctx["field_type"], "group");
    let sub_fields = ctx["sub_fields"].as_array().unwrap();
    assert_eq!(sub_fields.len(), 2);
    assert_eq!(sub_fields[0]["name"], "items[0][meta][author]");
    assert_eq!(sub_fields[0]["value"], "Alice");
    assert_eq!(sub_fields[1]["name"], "items[0][meta][published]");
    assert_eq!(sub_fields[1]["checked"], true);
}

#[test]
fn enriched_sub_field_empty_nested_array() {
    let mut inner_array = make_field("tags", FieldType::Array);
    inner_array.fields = vec![make_field("name", FieldType::Text)];

    // No data
    let ctx = build_enriched_sub_field_context(
        &inner_array,
        None,
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );

    assert_eq!(ctx["field_type"], "array");
    assert_eq!(ctx["row_count"], 0);
    let rows = ctx["rows"].as_array().unwrap();
    assert!(rows.is_empty());
}

#[test]
fn enriched_sub_field_select_preserves_selected() {
    let mut select_field = make_field("status", FieldType::Select);
    select_field.options = vec![
        SelectOption::new(LocalizedString::Plain("Draft".to_string()), "draft"),
        SelectOption::new(LocalizedString::Plain("Published".to_string()), "published"),
    ];

    let raw_value = serde_json::json!("published");

    let ctx = build_enriched_sub_field_context(
        &select_field,
        Some(&raw_value),
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );

    let opts = ctx["options"].as_array().unwrap();
    assert_eq!(opts[0]["selected"], false);
    assert_eq!(opts[1]["selected"], true);
}

#[test]
fn max_depth_prevents_infinite_recursion() {
    // Build a deeply nested array structure
    fn make_nested_array(depth: usize) -> FieldDefinition {
        let mut field =
            FieldDefinition::builder(format!("level{}", depth), FieldType::Array).build();
        if depth < 10 {
            field.fields = vec![make_nested_array(depth + 1)];
        } else {
            field.fields = vec![FieldDefinition::builder("leaf", FieldType::Text).build()];
        }
        field
    }
    let deep = make_nested_array(0);
    let fields = vec![deep];
    // This should not stack overflow -- MAX_FIELD_DEPTH caps recursion
    let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0]["field_type"], "array");
}

// --- enriched_sub_field: error propagation ---

#[test]
fn enriched_sub_field_with_error() {
    let sf = make_field("title", FieldType::Text);
    let mut errors = HashMap::new();
    errors.insert("content[0][title]".to_string(), "Required".to_string());
    let ctx = build_enriched_sub_field_context(
        &sf,
        Some(&serde_json::json!("val")),
        "content",
        0,
        &SubFieldOpts::builder(&errors).depth(1).build(),
    );
    assert_eq!(ctx["error"], "Required");
}

// --- enriched_sub_field: max depth ---

#[test]
fn enriched_sub_field_max_depth_returns_early() {
    let mut arr = make_field("deep", FieldType::Array);
    arr.fields = vec![make_field("leaf", FieldType::Text)];
    let ctx = build_enriched_sub_field_context(
        &arr,
        Some(&serde_json::json!([])),
        "parent",
        0,
        &SubFieldOpts::builder(&HashMap::new())
            .depth(MAX_FIELD_DEPTH)
            .build(),
    );
    // At max depth, array-specific fields should not be added
    assert!(ctx.get("rows").is_none());
    assert!(ctx.get("sub_fields").is_none());
}

// --- enriched_sub_field: date field ---

#[test]
fn enriched_sub_field_date_day_only() {
    let sf = make_field("d", FieldType::Date);
    let raw = serde_json::json!("2026-03-15T10:00:00Z");
    let ctx = build_enriched_sub_field_context(
        &sf,
        Some(&raw),
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );
    assert_eq!(ctx["picker_appearance"], "dayOnly");
    assert_eq!(ctx["date_only_value"], "2026-03-15");
}

#[test]
fn enriched_sub_field_date_day_and_time() {
    let mut sf = make_field("d", FieldType::Date);
    sf.picker_appearance = Some("dayAndTime".to_string());
    let raw = serde_json::json!("2026-03-15T10:30:00Z");
    let ctx = build_enriched_sub_field_context(
        &sf,
        Some(&raw),
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );
    assert_eq!(ctx["picker_appearance"], "dayAndTime");
    assert_eq!(ctx["datetime_local_value"], "2026-03-15T10:30");
}

#[test]
fn enriched_sub_field_date_short_value() {
    let sf = make_field("d", FieldType::Date);
    let raw = serde_json::json!("short");
    let ctx = build_enriched_sub_field_context(
        &sf,
        Some(&raw),
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );
    assert_eq!(ctx["date_only_value"], "short");
}

// --- enriched_sub_field: upload field ---

#[test]
fn enriched_sub_field_upload() {
    use crate::core::field::RelationshipConfig;
    let mut sf = make_field("image", FieldType::Upload);
    sf.relationship = Some(RelationshipConfig::new("media", false));
    let ctx = build_enriched_sub_field_context(
        &sf,
        Some(&serde_json::json!("img123")),
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );
    assert_eq!(ctx["relationship_collection"], "media");
    assert_eq!(ctx["picker"], "drawer");
}

// --- enriched_sub_field: relationship field ---

#[test]
fn enriched_sub_field_relationship() {
    use crate::core::field::RelationshipConfig;
    let mut sf = make_field("author", FieldType::Relationship);
    sf.relationship = Some(RelationshipConfig::new("users", true));
    let ctx = build_enriched_sub_field_context(
        &sf,
        Some(&serde_json::json!("user1")),
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );
    assert_eq!(ctx["relationship_collection"], "users");
    assert_eq!(ctx["has_many"], true);
}

// --- enriched_sub_field: value stringification ---

#[test]
fn enriched_sub_field_null_value_empty_string() {
    let sf = make_field("title", FieldType::Text);
    let ctx = build_enriched_sub_field_context(
        &sf,
        Some(&serde_json::Value::Null),
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );
    assert_eq!(ctx["value"], "");
}

#[test]
fn enriched_sub_field_number_to_string() {
    let sf = make_field("count", FieldType::Number);
    let ctx = build_enriched_sub_field_context(
        &sf,
        Some(&serde_json::json!(42)),
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );
    assert_eq!(ctx["value"], "42");
}

#[test]
fn enriched_sub_field_no_value() {
    let sf = make_field("title", FieldType::Text);
    let ctx = build_enriched_sub_field_context(
        &sf,
        None,
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );
    assert_eq!(ctx["value"], "");
}

// --- enriched_sub_field: array with min/max rows, collapsed, labels ---

#[test]
fn enriched_sub_field_array_with_options() {
    let mut arr = make_field("tags", FieldType::Array);
    arr.fields = vec![make_field("name", FieldType::Text)];
    arr.min_rows = Some(1);
    arr.max_rows = Some(5);
    arr.admin.collapsed = true;
    arr.admin.labels_singular = Some(LocalizedString::Plain("Tag".to_string()));
    let ctx = build_enriched_sub_field_context(
        &arr,
        Some(&serde_json::json!([])),
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );
    assert_eq!(ctx["min_rows"], 1);
    assert_eq!(ctx["max_rows"], 5);
    assert_eq!(ctx["init_collapsed"], true);
    assert_eq!(ctx["add_label"], "Tag");
}

// --- enriched_sub_field: blocks with min/max rows, collapsed, labels ---

#[test]
fn enriched_sub_field_blocks_with_options() {
    let mut blk = make_field("sections", FieldType::Blocks);
    blk.blocks = vec![BlockDefinition::new(
        "text",
        vec![make_field("body", FieldType::Text)],
    )];
    blk.min_rows = Some(0);
    blk.max_rows = Some(10);
    blk.admin.collapsed = true;
    blk.admin.labels_singular = Some(LocalizedString::Plain("Section".to_string()));
    blk.admin.label_field = Some("body".to_string());
    let ctx = build_enriched_sub_field_context(
        &blk,
        Some(&serde_json::json!([])),
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );
    assert_eq!(ctx["min_rows"], 0);
    assert_eq!(ctx["max_rows"], 10);
    assert_eq!(ctx["init_collapsed"], true);
    assert_eq!(ctx["add_label"], "Section");
    assert_eq!(ctx["label_field"], "body");
}

// --- enriched_sub_field: nested blocks with row errors ---

#[test]
fn enriched_sub_field_nested_array_row_errors() {
    let mut inner_array = make_field("items", FieldType::Array);
    inner_array.fields = vec![make_field("title", FieldType::Text)];

    let raw_value = serde_json::json!([{"title": ""}]);
    let mut errors = HashMap::new();
    errors.insert(
        "parent[0][items][0][title]".to_string(),
        "Required".to_string(),
    );

    let ctx = build_enriched_sub_field_context(
        &inner_array,
        Some(&raw_value),
        "parent",
        0,
        &SubFieldOpts::builder(&errors).depth(1).build(),
    );

    let rows = ctx["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let row_fields = rows[0]["sub_fields"].as_array().unwrap();
    assert_eq!(row_fields[0]["error"], "Required");
    assert_eq!(rows[0]["has_errors"], true);
}

#[test]
fn enriched_sub_field_nested_blocks_row_errors() {
    let mut blk = make_field("sections", FieldType::Blocks);
    blk.blocks = vec![{
        let mut bd = BlockDefinition::new("text", vec![make_field("body", FieldType::Richtext)]);
        bd.label = Some(LocalizedString::Plain("Text".to_string()));
        bd
    }];

    let raw_value = serde_json::json!([{"_block_type": "text", "body": ""}]);
    let mut errors = HashMap::new();
    errors.insert(
        "parent[0][sections][0][body]".to_string(),
        "Required".to_string(),
    );

    let ctx = build_enriched_sub_field_context(
        &blk,
        Some(&raw_value),
        "parent",
        0,
        &SubFieldOpts::builder(&errors).depth(1).build(),
    );

    let rows = ctx["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["has_errors"], true);
}

// --- enriched_sub_field: group with collapsed ---

#[test]
fn enriched_sub_field_group_collapsed() {
    let mut grp = make_field("meta", FieldType::Group);
    grp.fields = vec![make_field("author", FieldType::Text)];
    grp.admin.collapsed = true;
    let raw = serde_json::json!({"author": "Alice"});
    let ctx = build_enriched_sub_field_context(
        &grp,
        Some(&raw),
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );
    assert_eq!(ctx["collapsed"], true);
}

// --- enriched_sub_field: group with non-object value ---

#[test]
fn enriched_sub_field_group_with_null_value() {
    let mut grp = make_field("meta", FieldType::Group);
    grp.fields = vec![make_field("author", FieldType::Text)];
    let ctx = build_enriched_sub_field_context(
        &grp,
        Some(&serde_json::Value::Null),
        "items",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );
    // group_obj should be None so nested values are empty
    let sub_fields = ctx["sub_fields"].as_array().unwrap();
    assert_eq!(sub_fields[0]["value"], "");
}

// --- enriched_sub_field: nested blocks with unknown block type ---

#[test]
fn enriched_sub_field_nested_blocks_unknown_type() {
    let mut blk = make_field("sections", FieldType::Blocks);
    blk.blocks = vec![{
        let mut bd = BlockDefinition::new("text", vec![make_field("body", FieldType::Richtext)]);
        bd.label = Some(LocalizedString::Plain("Text".to_string()));
        bd
    }];

    // Row with unknown block type
    let raw_value = serde_json::json!([{"_block_type": "unknown_type", "body": "content"}]);

    let ctx = build_enriched_sub_field_context(
        &blk,
        Some(&raw_value),
        "parent",
        0,
        &SubFieldOpts::builder(&HashMap::new()).depth(1).build(),
    );

    let rows = ctx["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["_block_type"], "unknown_type");
    assert_eq!(rows[0]["block_label"], "unknown_type"); // falls back to block_type string
    // sub_fields should be empty since block_def is not found
    let sub_fields = rows[0]["sub_fields"].as_array().unwrap();
    assert!(sub_fields.is_empty());
}

// --- enrich_nested_fields tests ---

#[test]
fn enrich_nested_fields_upload_gets_options() {
    use crate::core::collection::*;
    use crate::core::field::RelationshipConfig;

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE media (
            id TEXT PRIMARY KEY,
            alt TEXT,
            caption TEXT,
            filename TEXT,
            mime_type TEXT,
            url TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        INSERT INTO media (id, alt, filename, mime_type, url, created_at, updated_at)
        VALUES ('img1', 'Logo', 'logo.png', 'image/png', '/uploads/media/logo.png', '2024-01-01', '2024-01-01');
        INSERT INTO media (id, alt, filename, mime_type, url, created_at, updated_at)
        VALUES ('img2', 'Banner', 'banner.jpg', 'image/jpeg', '/uploads/media/banner.jpg', '2024-01-01', '2024-01-01');"
    ).unwrap();

    let mut media_def = CollectionDefinition::new("media");
    media_def.timestamps = true;
    media_def.fields = vec![
        make_field("alt", FieldType::Text),
        make_field("caption", FieldType::Text),
        make_field("filename", FieldType::Text),
        make_field("mime_type", FieldType::Text),
        make_field("url", FieldType::Text),
    ];
    let mut upload_config = crate::core::upload::CollectionUpload::default();
    upload_config.enabled = true;
    upload_config.mime_types = vec!["image/*".to_string()];
    media_def.upload = Some(upload_config);

    let mut registry = crate::core::Registry::new();
    registry.register_collection(media_def);

    let mut upload_field = make_field("image", FieldType::Upload);
    upload_field.relationship = Some(RelationshipConfig::new("media", false));

    let field_defs = vec![upload_field];
    let mut sub_fields = vec![serde_json::json!({
        "name": "content[0][image]",
        "field_type": "upload",
        "value": "img1",
        "relationship_collection": "media",
    })];

    enrich_nested_fields(&mut sub_fields, &field_defs, &conn, &registry, None);

    let items = sub_fields[0]["selected_items"]
        .as_array()
        .expect("selected_items should be populated");
    assert_eq!(items.len(), 1, "Should have 1 selected item");
    assert_eq!(items[0]["id"], "img1");
    assert_eq!(items[0]["label"], "logo.png");
}

#[test]
fn enrich_nested_fields_relationship_gets_options() {
    use crate::core::collection::*;
    use crate::core::field::RelationshipConfig;

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE users (
            id TEXT PRIMARY KEY,
            name TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        INSERT INTO users (id, name, created_at, updated_at)
        VALUES ('u1', 'Alice', '2024-01-01', '2024-01-01');
        INSERT INTO users (id, name, created_at, updated_at)
        VALUES ('u2', 'Bob', '2024-01-01', '2024-01-01');",
    )
    .unwrap();

    let mut users_def = CollectionDefinition::new("users");
    users_def.timestamps = true;
    users_def.fields = vec![make_field("name", FieldType::Text)];
    users_def.admin.use_as_title = Some("name".to_string());

    let mut registry = crate::core::Registry::new();
    registry.register_collection(users_def);

    let mut rel_field = make_field("author", FieldType::Relationship);
    rel_field.relationship = Some(RelationshipConfig::new("users", false));

    let field_defs = vec![rel_field];
    let mut sub_fields = vec![serde_json::json!({
        "name": "items[0][author]",
        "field_type": "relationship",
        "value": "u1",
        "relationship_collection": "users",
    })];

    enrich_nested_fields(&mut sub_fields, &field_defs, &conn, &registry, None);

    let items = sub_fields[0]["selected_items"]
        .as_array()
        .expect("selected_items should be populated");
    assert_eq!(items.len(), 1, "Should have 1 selected item");
    assert_eq!(items[0]["id"], "u1");
    assert_eq!(items[0]["label"], "Alice");
}

#[test]
fn enrich_nested_fields_recurses_into_layout() {
    use crate::core::collection::*;
    use crate::core::field::RelationshipConfig;

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE tags (
            id TEXT PRIMARY KEY,
            label TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        INSERT INTO tags (id, label, created_at, updated_at)
        VALUES ('t1', 'Rust', '2024-01-01', '2024-01-01');",
    )
    .unwrap();

    let mut tags_def = CollectionDefinition::new("tags");
    tags_def.timestamps = true;
    tags_def.fields = vec![make_field("label", FieldType::Text)];
    tags_def.admin.use_as_title = Some("label".to_string());

    let mut registry = crate::core::Registry::new();
    registry.register_collection(tags_def);

    // A Row containing a Relationship field
    let mut rel_field = make_field("tag", FieldType::Relationship);
    rel_field.relationship = Some(RelationshipConfig::new("tags", false));
    let row_field = FieldDefinition::builder("row1", FieldType::Row)
        .fields(vec![rel_field])
        .build();

    let field_defs = vec![row_field];
    let mut sub_fields = vec![serde_json::json!({
        "name": "row1",
        "field_type": "row",
        "sub_fields": [{
            "name": "tag",
            "field_type": "relationship",
            "value": "",
            "relationship_collection": "tags",
        }],
    })];

    enrich_nested_fields(&mut sub_fields, &field_defs, &conn, &registry, None);

    let row_subs = sub_fields[0]["sub_fields"].as_array().unwrap();
    // Empty value → selected_items is empty array
    let items = row_subs[0]["selected_items"]
        .as_array()
        .expect("Nested relationship inside Row should be enriched");
    assert_eq!(
        items.len(),
        0,
        "Empty value should produce empty selected_items"
    );
}

#[test]
fn enrich_nested_fields_blocks_template_gets_upload_options() {
    // Regression: block definition templates (for new rows) must have upload options enriched
    use crate::core::collection::*;
    use crate::core::field::RelationshipConfig;

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE media (
            id TEXT PRIMARY KEY,
            filename TEXT,
            mime_type TEXT,
            url TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        INSERT INTO media (id, filename, mime_type, url, created_at, updated_at)
        VALUES ('m1', 'photo.jpg', 'image/jpeg', '/uploads/photo.jpg', '2024-01-01', '2024-01-01');"
    ).unwrap();

    let mut media_def = CollectionDefinition::new("media");
    media_def.timestamps = true;
    media_def.fields = vec![
        make_field("filename", FieldType::Text),
        make_field("mime_type", FieldType::Text),
        make_field("url", FieldType::Text),
    ];
    let mut upload_config = crate::core::upload::CollectionUpload::default();
    upload_config.enabled = true;
    media_def.upload = Some(upload_config);

    let mut registry = crate::core::Registry::new();
    registry.register_collection(media_def);

    // A Blocks field with an "image" block containing an upload field
    let mut upload_field = make_field("image", FieldType::Upload);
    upload_field.relationship = Some(RelationshipConfig::new("media", false));
    let mut blocks_field = FieldDefinition::builder("content", FieldType::Blocks).build();
    blocks_field.blocks = vec![BlockDefinition::new("image", vec![upload_field])];

    let field_defs = vec![blocks_field];
    // Simulate the block_definitions context (as built by build_single_field_context)
    let mut sub_fields = vec![serde_json::json!({
        "name": "content",
        "field_type": "blocks",
        "block_definitions": [{
            "block_type": "image",
            "label": "Image",
            "fields": [{
                "name": "content[__INDEX__][image]",
                "field_type": "upload",
                "value": "",
                "relationship_collection": "media",
            }],
        }],
        "rows": [],
    })];

    enrich_nested_fields(&mut sub_fields, &field_defs, &conn, &registry, None);

    let block_defs = sub_fields[0]["block_definitions"].as_array().unwrap();
    let fields = block_defs[0]["fields"].as_array().unwrap();
    // Empty value → selected_items is empty array (no full table scan)
    let items = fields[0]["selected_items"]
        .as_array()
        .expect("Upload inside block template should have selected_items");
    assert_eq!(
        items.len(),
        0,
        "Empty value should produce empty selected_items"
    );
}

#[test]
fn enrich_nested_fields_array_template_gets_upload_options() {
    // Regression: array sub_fields template (for new rows) must have upload options enriched
    use crate::core::collection::*;
    use crate::core::field::RelationshipConfig;

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE media (
            id TEXT PRIMARY KEY,
            filename TEXT,
            mime_type TEXT,
            url TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        INSERT INTO media (id, filename, mime_type, url, created_at, updated_at)
        VALUES ('m1', 'doc.pdf', 'application/pdf', '/uploads/doc.pdf', '2024-01-01', '2024-01-01');"
    ).unwrap();

    let mut media_def = CollectionDefinition::new("media");
    media_def.timestamps = true;
    media_def.fields = vec![
        make_field("filename", FieldType::Text),
        make_field("mime_type", FieldType::Text),
        make_field("url", FieldType::Text),
    ];
    let mut upload_config = crate::core::upload::CollectionUpload::default();
    upload_config.enabled = true;
    media_def.upload = Some(upload_config);

    let mut registry = crate::core::Registry::new();
    registry.register_collection(media_def);

    let mut upload_field = make_field("file", FieldType::Upload);
    upload_field.relationship = Some(RelationshipConfig::new("media", false));
    let array_field = FieldDefinition::builder("attachments", FieldType::Array)
        .fields(vec![upload_field])
        .build();

    let field_defs = vec![array_field];
    let mut sub_fields = vec![serde_json::json!({
        "name": "attachments",
        "field_type": "array",
        "sub_fields": [{
            "name": "attachments[__INDEX__][file]",
            "field_type": "upload",
            "value": "",
            "relationship_collection": "media",
        }],
        "rows": [],
    })];

    enrich_nested_fields(&mut sub_fields, &field_defs, &conn, &registry, None);

    let template_fields = sub_fields[0]["sub_fields"].as_array().unwrap();
    // Empty value → selected_items is empty array (no full table scan)
    let items = template_fields[0]["selected_items"]
        .as_array()
        .expect("Upload inside array template should have selected_items");
    assert_eq!(
        items.len(),
        0,
        "Empty value should produce empty selected_items"
    );
}

#[test]
fn enrich_field_contexts_blocks_inside_tabs_populates_rows() {
    // Regression: blocks inside Tabs were not populated from doc_fields because
    // enrich_field_contexts delegated to enrich_nested_fields instead of recursing.
    use crate::core::field::BlockDefinition;

    let mut blocks_field = FieldDefinition::builder("content", FieldType::Blocks).build();
    blocks_field.blocks = vec![{
        let mut bd = BlockDefinition::new("hero", vec![make_field("heading", FieldType::Text)]);
        bd.label = Some(LocalizedString::Plain("Hero".to_string()));
        bd
    }];
    let mut tabs_field = FieldDefinition::builder("page_settings", FieldType::Tabs).build();
    tabs_field.tabs = vec![crate::core::field::FieldTab::new(
        "Content",
        vec![blocks_field.clone()],
    )];
    let field_defs = vec![tabs_field];

    // Build initial field contexts (like the template would)
    let values = HashMap::new();
    let errors = HashMap::new();
    let mut contexts = build_field_contexts(&field_defs, &values, &errors, false, false);

    // Simulate doc_fields with blocks data (as hydrate_document would produce)
    let mut doc_fields: HashMap<String, serde_json::Value> = HashMap::new();
    doc_fields.insert(
        "content".to_string(),
        serde_json::json!([
            {"_block_type": "hero", "heading": "Welcome"},
        ]),
    );

    // Construct a minimal AdminState for the test
    let tmp = tempfile::tempdir().unwrap();
    let manager = r2d2_sqlite::SqliteConnectionManager::memory();
    let pool =
        crate::db::DbPool::from_pool(r2d2::Pool::builder().max_size(4).build(manager).unwrap());
    let shared_reg = std::sync::Arc::new(std::sync::RwLock::new(
        crate::core::registry::Registry::default(),
    ));
    let config = crate::config::CrapConfig::default();
    let hook_runner = crate::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(shared_reg.clone())
        .config(&config)
        .build()
        .unwrap();
    let registry = std::sync::Arc::new(shared_reg.read().unwrap().clone());
    let hbs = std::sync::Arc::new(handlebars::Handlebars::new());
    let email_renderer =
        std::sync::Arc::new(crate::core::email::EmailRenderer::new(tmp.path()).unwrap());
    let login_limiter = std::sync::Arc::new(crate::core::rate_limit::LoginRateLimiter::new(5, 300));
    let translations =
        std::sync::Arc::new(crate::admin::translations::Translations::load(tmp.path()));
    let state = crate::admin::AdminState {
        config,
        config_dir: tmp.path().to_path_buf(),
        pool,
        registry,
        handlebars: hbs,
        hook_runner,
        jwt_secret: "test".into(),
        email_renderer,
        event_bus: None,
        login_limiter,
        forgot_password_limiter: std::sync::Arc::new(
            crate::core::rate_limit::LoginRateLimiter::new(3, 900),
        ),
        has_auth: false,
        translations,
        shutdown: tokio_util::sync::CancellationToken::new(),
    };

    // Call enrich_field_contexts — the fix ensures Tabs recurse into Blocks
    enrich_field_contexts(
        &mut contexts,
        &field_defs,
        &doc_fields,
        &state,
        &EnrichOptions::builder(&errors).build(),
    );

    // Verify: the blocks field inside the first tab should have populated rows
    let tabs = contexts[0]["tabs"].as_array().unwrap();
    let tab_sub_fields = tabs[0]["sub_fields"].as_array().unwrap();
    let blocks_ctx = &tab_sub_fields[0];
    assert_eq!(blocks_ctx["field_type"], "blocks");
    let rows = blocks_ctx["rows"]
        .as_array()
        .expect("blocks inside Tabs must have rows populated from doc_fields");
    assert_eq!(rows.len(), 1, "should have 1 block row");
    assert_eq!(rows[0]["_block_type"], "hero");
}

#[test]
fn enrich_field_contexts_array_inside_row_populates_rows() {
    // Regression: arrays inside Row were not populated from doc_fields
    let array_field = FieldDefinition::builder("items", FieldType::Array)
        .fields(vec![make_field("label", FieldType::Text)])
        .build();
    let row_field = FieldDefinition::builder("main_row", FieldType::Row)
        .fields(vec![array_field.clone()])
        .build();
    let field_defs = vec![row_field];

    let values = HashMap::new();
    let errors = HashMap::new();
    let mut contexts = build_field_contexts(&field_defs, &values, &errors, false, false);

    let mut doc_fields: HashMap<String, serde_json::Value> = HashMap::new();
    doc_fields.insert(
        "items".to_string(),
        serde_json::json!([
            {"label": "First"},
            {"label": "Second"},
        ]),
    );

    let tmp = tempfile::tempdir().unwrap();
    let manager = r2d2_sqlite::SqliteConnectionManager::memory();
    let pool =
        crate::db::DbPool::from_pool(r2d2::Pool::builder().max_size(4).build(manager).unwrap());
    let shared_reg = std::sync::Arc::new(std::sync::RwLock::new(
        crate::core::registry::Registry::default(),
    ));
    let config = crate::config::CrapConfig::default();
    let hook_runner = crate::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(shared_reg.clone())
        .config(&config)
        .build()
        .unwrap();
    let registry = std::sync::Arc::new(shared_reg.read().unwrap().clone());
    let hbs = std::sync::Arc::new(handlebars::Handlebars::new());
    let email_renderer =
        std::sync::Arc::new(crate::core::email::EmailRenderer::new(tmp.path()).unwrap());
    let login_limiter = std::sync::Arc::new(crate::core::rate_limit::LoginRateLimiter::new(5, 300));
    let translations =
        std::sync::Arc::new(crate::admin::translations::Translations::load(tmp.path()));
    let state = crate::admin::AdminState {
        config,
        config_dir: tmp.path().to_path_buf(),
        pool,
        registry,
        handlebars: hbs,
        hook_runner,
        jwt_secret: "test".into(),
        email_renderer,
        event_bus: None,
        login_limiter,
        forgot_password_limiter: std::sync::Arc::new(
            crate::core::rate_limit::LoginRateLimiter::new(3, 900),
        ),
        has_auth: false,
        translations,
        shutdown: tokio_util::sync::CancellationToken::new(),
    };

    enrich_field_contexts(
        &mut contexts,
        &field_defs,
        &doc_fields,
        &state,
        &EnrichOptions::builder(&errors).build(),
    );

    let row_sub_fields = contexts[0]["sub_fields"].as_array().unwrap();
    let array_ctx = &row_sub_fields[0];
    assert_eq!(array_ctx["field_type"], "array");
    let rows = array_ctx["rows"]
        .as_array()
        .expect("array inside Row must have rows populated from doc_fields");
    assert_eq!(rows.len(), 2, "should have 2 array rows");
}

// ── Layout wrappers inside Array: transparent names and data ─────────

fn make_test_state() -> crate::admin::AdminState {
    let tmp = tempfile::tempdir().unwrap();
    let manager = r2d2_sqlite::SqliteConnectionManager::memory();
    let pool =
        crate::db::DbPool::from_pool(r2d2::Pool::builder().max_size(4).build(manager).unwrap());
    let shared_reg = std::sync::Arc::new(std::sync::RwLock::new(
        crate::core::registry::Registry::default(),
    ));
    let config = crate::config::CrapConfig::default();
    let hook_runner = crate::hooks::lifecycle::HookRunner::builder()
        .config_dir(tmp.path())
        .registry(shared_reg.clone())
        .config(&config)
        .build()
        .unwrap();
    let registry = std::sync::Arc::new(shared_reg.read().unwrap().clone());
    let hbs = std::sync::Arc::new(handlebars::Handlebars::new());
    let email_renderer =
        std::sync::Arc::new(crate::core::email::EmailRenderer::new(tmp.path()).unwrap());
    let login_limiter = std::sync::Arc::new(crate::core::rate_limit::LoginRateLimiter::new(5, 300));
    let translations =
        std::sync::Arc::new(crate::admin::translations::Translations::load(tmp.path()));
    crate::admin::AdminState {
        config,
        config_dir: tmp.path().to_path_buf(),
        pool,
        registry,
        handlebars: hbs,
        hook_runner,
        jwt_secret: "test".into(),
        email_renderer,
        event_bus: None,
        login_limiter,
        forgot_password_limiter: std::sync::Arc::new(
            crate::core::rate_limit::LoginRateLimiter::new(3, 900),
        ),
        has_auth: false,
        translations,
        shutdown: tokio_util::sync::CancellationToken::new(),
    }
}

#[test]
fn enriched_sub_field_tabs_in_array_transparent_names() {
    use crate::core::field::FieldTab;

    // Array "items" with sub-fields inside a Tabs wrapper
    let mut arr_field = make_field("items", FieldType::Array);
    arr_field.fields = vec![
        FieldDefinition::builder("layout", FieldType::Tabs)
            .tabs(vec![
                FieldTab::new("General", vec![make_field("title", FieldType::Text)]),
                FieldTab::new("Content", vec![make_field("body", FieldType::Textarea)]),
            ])
            .build(),
    ];

    // Simulate hydrated data: flat JSON (as it comes from the join table)
    let row_data = serde_json::json!([
        {"id": "r1", "title": "Hello", "body": "World"}
    ]);

    let fields = vec![arr_field.clone()];
    let values = HashMap::new();
    let errors = HashMap::new();
    let mut contexts = build_field_contexts(&fields, &values, &errors, false, false);

    let mut doc_fields = HashMap::new();
    doc_fields.insert("items".to_string(), row_data);

    let state = make_test_state();

    enrich_field_contexts(
        &mut contexts,
        &fields,
        &doc_fields,
        &state,
        &EnrichOptions::builder(&errors).build(),
    );

    // The array row should contain a Tabs sub-field whose tabs contain the actual fields
    let rows = contexts[0]["rows"].as_array().expect("should have rows");
    assert_eq!(rows.len(), 1);

    let row_sub_fields = rows[0]["sub_fields"].as_array().unwrap();
    // The sub_fields should contain the Tabs wrapper
    assert_eq!(row_sub_fields.len(), 1);
    assert_eq!(row_sub_fields[0]["field_type"], "tabs");

    // The Tabs wrapper's name should be transparent: items[0] (not items[0][layout])
    assert_eq!(row_sub_fields[0]["name"], "items[0]");

    // Check that tab children have correct transparent names and data
    let tabs = row_sub_fields[0]["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 2);

    let tab1_fields = tabs[0]["sub_fields"].as_array().unwrap();
    assert_eq!(tab1_fields[0]["name"], "items[0][title]");
    assert_eq!(tab1_fields[0]["value"], "Hello");

    let tab2_fields = tabs[1]["sub_fields"].as_array().unwrap();
    assert_eq!(tab2_fields[0]["name"], "items[0][body]");
    assert_eq!(tab2_fields[0]["value"], "World");
}

#[test]
fn enriched_sub_field_row_in_array_transparent_names() {
    // Array "items" with sub-fields inside a Row wrapper
    let mut arr_field = make_field("items", FieldType::Array);
    arr_field.fields = vec![
        FieldDefinition::builder("row_wrap", FieldType::Row)
            .fields(vec![
                make_field("x", FieldType::Text),
                make_field("y", FieldType::Text),
            ])
            .build(),
    ];

    let row_data = serde_json::json!([
        {"id": "r1", "x": "10", "y": "20"}
    ]);

    let fields = vec![arr_field.clone()];
    let values = HashMap::new();
    let errors = HashMap::new();
    let mut contexts = build_field_contexts(&fields, &values, &errors, false, false);

    let mut doc_fields = HashMap::new();
    doc_fields.insert("items".to_string(), row_data);

    let state = make_test_state();

    enrich_field_contexts(
        &mut contexts,
        &fields,
        &doc_fields,
        &state,
        &EnrichOptions::builder(&errors).build(),
    );

    let rows = contexts[0]["rows"].as_array().expect("should have rows");
    assert_eq!(rows.len(), 1);

    let row_sub_fields = rows[0]["sub_fields"].as_array().unwrap();
    assert_eq!(row_sub_fields.len(), 1);
    assert_eq!(row_sub_fields[0]["field_type"], "row");

    // Transparent name: items[0] (not items[0][row_wrap])
    assert_eq!(row_sub_fields[0]["name"], "items[0]");

    // Children have correct names and data
    let children = row_sub_fields[0]["sub_fields"].as_array().unwrap();
    assert_eq!(children[0]["name"], "items[0][x]");
    assert_eq!(children[0]["value"], "10");
    assert_eq!(children[1]["name"], "items[0][y]");
    assert_eq!(children[1]["value"], "20");
}

#[test]
fn enriched_sub_field_row_inside_tabs_in_array_transparent_names() {
    use crate::core::field::FieldTab;

    // Array "team_members" with Tabs containing Rows (double nesting)
    let mut arr_field = make_field("team_members", FieldType::Array);
    arr_field.fields = vec![
        FieldDefinition::builder("member_tabs", FieldType::Tabs)
            .tabs(vec![
                FieldTab::new(
                    "Personal",
                    vec![
                        FieldDefinition::builder("name_row", FieldType::Row)
                            .fields(vec![
                                make_field("first_name", FieldType::Text),
                                make_field("last_name", FieldType::Text),
                            ])
                            .build(),
                        make_field("email", FieldType::Email),
                    ],
                ),
                FieldTab::new(
                    "Professional",
                    vec![make_field("job_title", FieldType::Text)],
                ),
            ])
            .build(),
    ];

    let row_data = serde_json::json!([
        {"id": "r1", "first_name": "John", "last_name": "Doe", "email": "john@example.com", "job_title": "Dev"}
    ]);

    let fields = vec![arr_field.clone()];
    let values = HashMap::new();
    let errors = HashMap::new();
    let mut contexts = build_field_contexts(&fields, &values, &errors, false, false);

    let mut doc_fields = HashMap::new();
    doc_fields.insert("team_members".to_string(), row_data);

    let state = make_test_state();

    enrich_field_contexts(
        &mut contexts,
        &fields,
        &doc_fields,
        &state,
        &EnrichOptions::builder(&errors).build(),
    );

    let rows = contexts[0]["rows"].as_array().expect("should have rows");
    assert_eq!(rows.len(), 1);

    // Top level: Tabs wrapper (transparent name)
    let row_sub_fields = rows[0]["sub_fields"].as_array().unwrap();
    assert_eq!(row_sub_fields.len(), 1);
    assert_eq!(row_sub_fields[0]["field_type"], "tabs");
    assert_eq!(row_sub_fields[0]["name"], "team_members[0]");

    let tabs = row_sub_fields[0]["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 2);

    // Personal tab: Row (transparent) + email
    let personal_fields = tabs[0]["sub_fields"].as_array().unwrap();
    assert_eq!(personal_fields.len(), 2);

    // Row wrapper should be transparent: team_members[0] (not team_members[0][name_row])
    assert_eq!(personal_fields[0]["field_type"], "row");
    assert_eq!(personal_fields[0]["name"], "team_members[0]");

    // Row children should be: team_members[0][first_name], team_members[0][last_name]
    let row_children = personal_fields[0]["sub_fields"].as_array().unwrap();
    assert_eq!(row_children[0]["name"], "team_members[0][first_name]");
    assert_eq!(row_children[0]["value"], "John");
    assert_eq!(row_children[1]["name"], "team_members[0][last_name]");
    assert_eq!(row_children[1]["value"], "Doe");

    // email field
    assert_eq!(personal_fields[1]["name"], "team_members[0][email]");
    assert_eq!(personal_fields[1]["value"], "john@example.com");

    // Professional tab: job_title
    let pro_fields = tabs[1]["sub_fields"].as_array().unwrap();
    assert_eq!(pro_fields[0]["name"], "team_members[0][job_title]");
    assert_eq!(pro_fields[0]["value"], "Dev");
}
