//! Composite fields: array and blocks rows, groups, tabs, nesting and the recursion guard.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    admin::handlers::field_context::test_helpers::{build_value_contexts, make_field},
    core::{
        BlockDefinition, FieldDefinition, FieldTab, FieldType, FieldWidth, LocalizedString,
        SelectOption as CoreSelectOption,
    },
};

// ── Array / Blocks sub-field enrichment ───────────────────────────

#[test]
fn build_field_contexts_array_sub_fields_include_type_and_label() {
    let mut arr_field = make_field("items", FieldType::Array);
    arr_field.fields = vec![
        make_field("title", FieldType::Text),
        make_field("body", FieldType::Richtext),
    ];
    let fields = vec![arr_field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result.len(), 1);
    let sub_fields = result[0]["sub_fields"].as_array().unwrap();
    assert_eq!(sub_fields.len(), 2);
    assert_eq!(sub_fields[0]["field_type"], "text");
    assert_eq!(sub_fields[0]["label"], "Title");
    assert_eq!(sub_fields[1]["field_type"], "richtext");
    assert_eq!(sub_fields[1]["label"], "Body");
}

#[test]
fn build_field_contexts_array_select_sub_field_includes_options() {
    let mut select_sf = make_field("status", FieldType::Select);
    select_sf.options = vec![
        CoreSelectOption::new(LocalizedString::Plain("Draft".to_string()), "draft"),
        CoreSelectOption::new(LocalizedString::Plain("Published".to_string()), "published"),
    ];
    let mut arr_field = make_field("items", FieldType::Array);
    arr_field.fields = vec![select_sf];
    let fields = vec![arr_field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    let sub_fields = result[0]["sub_fields"].as_array().unwrap();
    let opts = sub_fields[0]["options"].as_array().unwrap();
    assert_eq!(opts.len(), 2);
    assert_eq!(opts[0]["value"], "draft");
    assert_eq!(opts[1]["value"], "published");
}

#[test]
fn build_field_contexts_blocks_sub_fields_include_type_and_label() {
    let mut blocks_field = make_field("content", FieldType::Blocks);
    blocks_field.blocks = vec![{
        let mut bd = BlockDefinition::new(
            "rich",
            vec![
                make_field("heading", FieldType::Text),
                make_field("body", FieldType::Richtext),
            ],
        );
        bd.label = Some(LocalizedString::Plain("Rich Text".to_string()));
        bd
    }];
    let fields = vec![blocks_field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    let block_defs = result[0]["block_definitions"].as_array().unwrap();
    assert_eq!(block_defs.len(), 1);
    let block_fields = block_defs[0]["fields"].as_array().unwrap();
    assert_eq!(block_fields.len(), 2);
    assert_eq!(block_fields[0]["field_type"], "text");
    assert_eq!(block_fields[0]["label"], "Heading");
    assert_eq!(block_fields[1]["field_type"], "richtext");
    assert_eq!(block_fields[1]["label"], "Body");
}

#[test]
fn build_field_contexts_blocks_select_sub_field_includes_options() {
    let mut select_sf = make_field("align", FieldType::Select);
    select_sf.options = vec![
        CoreSelectOption::new(LocalizedString::Plain("Left".to_string()), "left"),
        CoreSelectOption::new(LocalizedString::Plain("Center".to_string()), "center"),
    ];
    let mut blocks_field = make_field("layout", FieldType::Blocks);
    blocks_field.blocks = vec![BlockDefinition::new("section", vec![select_sf])];
    let fields = vec![blocks_field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    let block_defs = result[0]["block_definitions"].as_array().unwrap();
    let block_fields = block_defs[0]["fields"].as_array().unwrap();
    let opts = block_fields[0]["options"].as_array().unwrap();
    assert_eq!(opts.len(), 2);
    assert_eq!(opts[0]["value"], "left");
    assert_eq!(opts[1]["value"], "center");
}

// ── Recursive composites: template_id, indexed names, deep nesting ─

#[test]
fn build_field_contexts_array_has_template_id() {
    let mut arr_field = make_field("items", FieldType::Array);
    arr_field.fields = vec![make_field("title", FieldType::Text)];
    let fields = vec![arr_field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["template_id"], "items");
}

#[test]
fn build_field_contexts_blocks_has_template_id() {
    let mut blocks_field = make_field("content", FieldType::Blocks);
    blocks_field.blocks = vec![BlockDefinition::new(
        "text",
        vec![make_field("body", FieldType::Text)],
    )];
    let fields = vec![blocks_field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["template_id"], "content");
}

#[test]
fn build_field_contexts_array_sub_fields_have_indexed_names() {
    let mut arr_field = make_field("slides", FieldType::Array);
    arr_field.fields = vec![
        make_field("title", FieldType::Text),
        make_field("body", FieldType::Textarea),
    ];
    let fields = vec![arr_field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    let sub_fields = result[0]["sub_fields"].as_array().unwrap();
    // Sub-fields in the template context should have __INDEX__ placeholder names.
    assert_eq!(sub_fields[0]["name"], "slides[__INDEX__][title]");
    assert_eq!(sub_fields[1]["name"], "slides[__INDEX__][body]");
}

#[test]
fn build_field_contexts_nested_array_in_blocks() {
    let mut inner_array = make_field("images", FieldType::Array);
    inner_array.fields = vec![
        make_field("url", FieldType::Text),
        make_field("caption", FieldType::Text),
    ];
    let mut blocks_field = make_field("content", FieldType::Blocks);
    blocks_field.blocks = vec![{
        let mut bd = BlockDefinition::new(
            "gallery",
            vec![make_field("title", FieldType::Text), inner_array],
        );
        bd.label = Some(LocalizedString::Plain("Gallery".to_string()));
        bd
    }];
    let fields = vec![blocks_field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

    let block_defs = result[0]["block_definitions"].as_array().unwrap();
    assert_eq!(block_defs.len(), 1);
    let block_fields = block_defs[0]["fields"].as_array().unwrap();
    assert_eq!(block_fields.len(), 2);

    assert_eq!(block_fields[0]["field_type"], "text");
    assert_eq!(block_fields[0]["name"], "content[__INDEX__][title]");

    assert_eq!(block_fields[1]["field_type"], "array");
    assert_eq!(block_fields[1]["name"], "content[__INDEX__][images]");

    let nested_sub_fields = block_fields[1]["sub_fields"].as_array().unwrap();
    assert_eq!(nested_sub_fields.len(), 2);
    assert_eq!(
        nested_sub_fields[0]["name"],
        "content[__INDEX__][images][__INDEX__][url]"
    );
    assert_eq!(
        nested_sub_fields[1]["name"],
        "content[__INDEX__][images][__INDEX__][caption]"
    );

    assert!(block_fields[1]["template_id"].as_str().is_some());
}

#[test]
fn build_field_contexts_nested_blocks_in_array() {
    let mut inner_blocks = make_field("sections", FieldType::Blocks);
    inner_blocks.blocks = vec![BlockDefinition::new(
        "text",
        vec![make_field("body", FieldType::Richtext)],
    )];
    let mut arr_field = make_field("pages", FieldType::Array);
    arr_field.fields = vec![make_field("title", FieldType::Text), inner_blocks];
    let fields = vec![arr_field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

    let sub_fields = result[0]["sub_fields"].as_array().unwrap();
    assert_eq!(sub_fields.len(), 2);
    assert_eq!(sub_fields[0]["field_type"], "text");
    assert_eq!(sub_fields[1]["field_type"], "blocks");

    let nested_block_defs = sub_fields[1]["block_definitions"].as_array().unwrap();
    assert_eq!(nested_block_defs.len(), 1);
    assert_eq!(nested_block_defs[0]["block_type"], "text");

    let nested_block_fields = nested_block_defs[0]["fields"].as_array().unwrap();
    assert_eq!(nested_block_fields[0]["field_type"], "richtext");
    assert_eq!(
        nested_block_fields[0]["name"],
        "pages[__INDEX__][sections][__INDEX__][body]"
    );
}

#[test]
fn build_field_contexts_nested_group_in_array() {
    let mut inner_group = make_field("meta", FieldType::Group);
    inner_group.fields = vec![
        make_field("author", FieldType::Text),
        make_field("date", FieldType::Date),
    ];
    let mut arr_field = make_field("entries", FieldType::Array);
    arr_field.fields = vec![inner_group];
    let fields = vec![arr_field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

    let sub_fields = result[0]["sub_fields"].as_array().unwrap();
    assert_eq!(sub_fields.len(), 1);
    assert_eq!(sub_fields[0]["field_type"], "group");

    let group_sub_fields = sub_fields[0]["sub_fields"].as_array().unwrap();
    assert_eq!(group_sub_fields.len(), 2);
    // A group nested in an array row indexes its children as
    // `<group>[0][field]` — the `[0]` the form parser requires to recognize
    // the group as a single object in a new row (see `construct_group` and
    // `single/composites.rs::group_in_array_row_indexes_children_with_zero`). Without it
    // a newly-added row's group data is dropped on save.
    assert_eq!(
        group_sub_fields[0]["name"],
        "entries[__INDEX__][meta][0][author]"
    );
    assert_eq!(
        group_sub_fields[1]["name"],
        "entries[__INDEX__][meta][0][date]"
    );
}

#[test]
fn build_field_contexts_nested_array_in_array() {
    let mut inner_array = make_field("tags", FieldType::Array);
    inner_array.fields = vec![make_field("name", FieldType::Text)];
    let mut outer_array = make_field("items", FieldType::Array);
    outer_array.fields = vec![make_field("title", FieldType::Text), inner_array];
    let fields = vec![outer_array];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

    let sub_fields = result[0]["sub_fields"].as_array().unwrap();
    assert_eq!(sub_fields[1]["field_type"], "array");

    let nested_sub = sub_fields[1]["sub_fields"].as_array().unwrap();
    assert_eq!(
        nested_sub[0]["name"],
        "items[__INDEX__][tags][__INDEX__][name]"
    );
}

// ── Group ─────────────────────────────────────────────────────────

#[test]
fn build_field_contexts_top_level_group_uses_double_underscore() {
    let mut group = make_field("seo", FieldType::Group);
    group.fields = vec![
        make_field("title", FieldType::Text),
        make_field("description", FieldType::Textarea),
    ];
    let fields = vec![group];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    let sub_fields = result[0]["sub_fields"].as_array().unwrap();
    assert_eq!(sub_fields[0]["name"], "seo__title");
    assert_eq!(sub_fields[1]["name"], "seo__description");
}

#[test]
fn build_field_contexts_group_collapsed() {
    let mut group = make_field("meta", FieldType::Group);
    group.admin.collapsed = true;
    group.fields = vec![make_field("author", FieldType::Text)];
    let fields = vec![group];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["collapsed"], true);
}

#[test]
fn build_field_contexts_group_sub_field_values() {
    let mut group = make_field("seo", FieldType::Group);
    group.fields = vec![make_field("title", FieldType::Text)];
    let mut values = HashMap::new();
    values.insert("seo__title".to_string(), "My SEO Title".to_string());
    let fields = vec![group];
    let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
    let sub_fields = result[0]["sub_fields"].as_array().unwrap();
    assert_eq!(sub_fields[0]["value"], "My SEO Title");
}

// ── Array admin options ───────────────────────────────────────────

#[test]
fn build_field_contexts_array_with_min_max_rows() {
    let mut arr = make_field("items", FieldType::Array);
    arr.fields = vec![make_field("title", FieldType::Text)];
    arr.min_rows = Some(1);
    arr.max_rows = Some(5);
    let fields = vec![arr];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["min_rows"], 1);
    assert_eq!(result[0]["max_rows"], 5);
}

#[test]
fn build_field_contexts_array_collapsed() {
    let mut arr = make_field("items", FieldType::Array);
    arr.fields = vec![make_field("title", FieldType::Text)];
    // collapsed defaults to true.
    let fields = vec![arr];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["init_collapsed"], true);

    // opt-out: collapsed = false.
    let mut arr2 = make_field("items", FieldType::Array);
    arr2.fields = vec![make_field("title", FieldType::Text)];
    arr2.admin.collapsed = false;
    let fields2 = vec![arr2];
    let result2 = build_value_contexts(&fields2, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result2[0]["init_collapsed"], false);
}

#[test]
fn build_field_contexts_array_labels_singular() {
    let mut arr = make_field("slides", FieldType::Array);
    arr.fields = vec![make_field("title", FieldType::Text)];
    arr.admin.labels.singular = Some(LocalizedString::Plain("Slide".to_string()));
    let fields = vec![arr];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["add_label"], "Slide");
}

/// Regression: `admin.labels.plural` was parsed but never read. It is the
/// field header when `admin.label` is not set; an explicit label wins.
#[test]
fn build_field_contexts_array_labels_plural_is_the_header() {
    let mut arr = make_field("slides_list", FieldType::Array);
    arr.fields = vec![make_field("title", FieldType::Text)];
    arr.admin.labels.plural = Some(LocalizedString::Plain("Slides".to_string()));
    let result = build_value_contexts(
        &[arr.clone()],
        &HashMap::new(),
        &HashMap::new(),
        false,
        false,
    );
    assert_eq!(result[0]["label"], "Slides");

    arr.admin.label = Some(LocalizedString::Plain("Carousel".to_string()));
    let result = build_value_contexts(&[arr], &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["label"], "Carousel");
}

/// Regression: `admin.rows` on code and JSON fields and `admin.width` on
/// any field never reached the edit form. Both render now, at the top
/// level and inside an array row's template.
#[test]
fn build_field_contexts_rows_and_width_reach_the_form() {
    let mut code = make_field("snippet", FieldType::Code);
    code.admin.rows = Some(4);
    let mut json = make_field("meta", FieldType::Json);
    json.admin.rows = Some(3);
    json.admin.width = Some(FieldWidth::Half);
    let plain_json = make_field("extra", FieldType::Json);
    let mut arr = make_field("items", FieldType::Array);
    let mut label = make_field("label", FieldType::Text);
    label.admin.width = Some(FieldWidth::Custom("40%".to_string()));
    arr.fields = vec![label];

    let result = build_value_contexts(
        &[code, json, plain_json, arr],
        &HashMap::new(),
        &HashMap::new(),
        false,
        false,
    );

    assert_eq!(result[0]["rows"], 4);
    assert!(result[0].get("width").is_none());
    assert_eq!(result[1]["rows"], 3);
    assert_eq!(result[1]["width"], "half");
    assert_eq!(result[2]["rows"], 12);
    assert_eq!(result[3]["sub_fields"][0]["width"], "custom");
    assert_eq!(result[3]["sub_fields"][0]["width_value"], "40%");
}

#[test]
fn build_field_contexts_array_label_field() {
    let mut arr = make_field("items", FieldType::Array);
    arr.fields = vec![make_field("title", FieldType::Text)];
    arr.admin.label_field = Some("title".to_string());
    let fields = vec![arr];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["label_field"], "title");
}

// ── Blocks admin options ──────────────────────────────────────────

#[test]
fn build_field_contexts_blocks_with_min_max_rows() {
    let mut blocks = make_field("content", FieldType::Blocks);
    blocks.blocks = vec![BlockDefinition::new(
        "text",
        vec![make_field("body", FieldType::Text)],
    )];
    blocks.min_rows = Some(1);
    blocks.max_rows = Some(10);
    let fields = vec![blocks];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["min_rows"], 1);
    assert_eq!(result[0]["max_rows"], 10);
}

#[test]
fn build_field_contexts_blocks_collapsed() {
    let mut blocks = make_field("content", FieldType::Blocks);
    blocks.blocks = vec![BlockDefinition::new(
        "text",
        vec![make_field("body", FieldType::Text)],
    )];
    let fields = vec![blocks];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["init_collapsed"], true);
}

#[test]
fn build_field_contexts_blocks_labels_singular() {
    let mut blocks = make_field("content", FieldType::Blocks);
    blocks.blocks = vec![BlockDefinition::new(
        "text",
        vec![make_field("body", FieldType::Text)],
    )];
    blocks.admin.labels.singular = Some(LocalizedString::Plain("Block".to_string()));
    let fields = vec![blocks];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["add_label"], "Block");
}

#[test]
fn build_field_contexts_blocks_block_label_field() {
    let mut blocks = make_field("content", FieldType::Blocks);
    blocks.blocks = vec![{
        let mut bd = BlockDefinition::new("text", vec![make_field("body", FieldType::Text)]);
        bd.label_field = Some("body".to_string());
        bd
    }];
    let fields = vec![blocks];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    let block_defs = result[0]["block_definitions"].as_array().unwrap();
    assert_eq!(block_defs[0]["label_field"], "body");
}

#[test]
fn build_field_contexts_blocks_group_and_image_url() {
    let mut blocks = make_field("content", FieldType::Blocks);
    blocks.blocks = vec![
        {
            let mut bd = BlockDefinition::new("hero", vec![]);
            bd.label = Some(LocalizedString::Plain("Hero".to_string()));
            bd.group = Some("Layout".to_string());
            bd.image_url = Some("/static/blocks/hero.svg".to_string());
            bd
        },
        {
            let mut bd = BlockDefinition::new("text", vec![]);
            bd.label = Some(LocalizedString::Plain("Text".to_string()));
            bd.group = Some("Content".to_string());
            bd
        },
        {
            let mut bd = BlockDefinition::new("divider", vec![]);
            bd.label = Some(LocalizedString::Plain("Divider".to_string()));
            bd
        },
    ];
    let fields = vec![blocks];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    let block_defs = result[0]["block_definitions"].as_array().unwrap();

    assert_eq!(block_defs[0]["group"], "Layout");
    assert_eq!(block_defs[0]["image_url"], "/static/blocks/hero.svg");

    assert_eq!(block_defs[1]["group"], "Content");
    assert!(block_defs[1].get("image_url").is_none_or(Value::is_null));

    assert!(block_defs[2].get("group").is_none_or(Value::is_null));
    assert!(block_defs[2].get("image_url").is_none_or(Value::is_null));
}

#[test]
fn build_field_contexts_blocks_picker_card() {
    let mut blocks = make_field("content", FieldType::Blocks);
    blocks.admin.picker = Some("card".to_string());
    blocks.blocks = vec![BlockDefinition::new(
        "text",
        vec![make_field("body", FieldType::Text)],
    )];
    let fields = vec![blocks];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["picker"], "card");
}

// ── has_many regression in composites ─────────────────────────────

/// Bug fix: `has_many` text inside a Group should produce `tags` /
/// `has_many` context.
#[test]
fn has_many_text_in_group_gets_tags_context() {
    let mut group = make_field("meta", FieldType::Group);
    let mut tags = make_field("tags", FieldType::Text);
    tags.has_many = true;
    group.fields = vec![tags];
    let fields = vec![group];

    let mut values = HashMap::new();
    values.insert("meta__tags".to_string(), r#"["rust","lua"]"#.to_string());
    let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);

    let sub = result[0]["sub_fields"].as_array().unwrap();
    assert_eq!(sub[0]["has_many"], true);
    let tags_arr = sub[0]["tags"].as_array().unwrap();
    assert_eq!(tags_arr.len(), 2);
    assert_eq!(tags_arr[0], "rust");
    assert_eq!(tags_arr[1], "lua");
    assert_eq!(sub[0]["value"], r#"["rust","lua"]"#);
}

// ── Tabs error_count ──────────────────────────────────────────────

#[test]
fn tabs_field_context_includes_error_count() {
    let mut tabs_field = make_field("settings", FieldType::Tabs);
    tabs_field.tabs = vec![
        FieldTab::new(
            "General",
            vec![
                {
                    let mut f = make_field("title", FieldType::Text);
                    f.required = true;
                    f
                },
                make_field("slug", FieldType::Text),
            ],
        ),
        FieldTab::new("Advanced", vec![make_field("meta", FieldType::Text)]),
    ];

    let values = HashMap::new();
    let mut errors = HashMap::new();
    errors.insert("title".to_string(), "Title is required".to_string());

    let result = build_value_contexts(&[tabs_field], &values, &errors, false, false);
    let tabs = result[0]["tabs"]
        .as_array()
        .expect("tabs should be an array");

    // First tab has 1 error (title is required).
    assert_eq!(tabs[0]["error_count"], 1);
    // Second tab has no errors.
    assert!(tabs[1].get("error_count").is_none() || tabs[1]["error_count"].is_null());
}

// ── Recursion guard ──────────────────────────────────────────────

#[test]
fn max_depth_prevents_infinite_recursion() {
    fn make_nested_array(depth: usize) -> FieldDefinition {
        let mut field = FieldDefinition::builder(format!("level{depth}"), FieldType::Array).build();
        if depth < 10 {
            field.fields = vec![make_nested_array(depth + 1)];
        } else {
            field.fields = vec![FieldDefinition::builder("leaf", FieldType::Text).build()];
        }
        field
    }
    let deep = make_nested_array(0);
    let fields = vec![deep];
    // Must not stack overflow — MAX_FIELD_DEPTH caps recursion.
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0]["field_type"], "array");
}
