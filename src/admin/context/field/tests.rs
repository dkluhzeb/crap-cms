//! Parity tests for the typed [`FieldContext`](super::FieldContext) variants.
//!
//! Each test constructs a representative instance and asserts the JSON shape
//! it serializes to matches what the existing
//! [`build_single_field_context`](crate::admin::handlers::field_context::builder)
//! produces. These pin the contract so 1.C.2.b can migrate the builder
//! without silently changing the wire format.

use serde_json::{Value, json};

use super::*;

/// Convenience: build a base with sensible defaults and the given name +
/// field_type. All other fields use parity-preserving defaults.
fn base(name: &str, field_type: &str) -> BaseFieldData {
    BaseFieldData {
        name: name.to_string(),
        field_type: field_type.to_string(),
        label: name.to_string(),
        required: false,
        value: Value::String(String::new()),
        placeholder: None,
        description: None,
        readonly: false,
        localized: false,
        locale_locked: false,
        position: None,
        error: None,
        validation: ValidationAttrs::default(),
        condition: ConditionData::default(),
    }
}

// ── Base shape ─────────────────────────────────────────────────────

#[test]
fn base_serializes_all_required_keys() {
    let f = TextField {
        base: base("title", "text"),
        has_many: None,
        tags: None,
    };
    let v = serde_json::to_value(FieldContext::Text(f)).unwrap();
    // All base keys present (placeholder/description as null per parity).
    assert_eq!(v["name"], "title");
    assert_eq!(v["field_type"], "text");
    assert_eq!(v["label"], "title");
    assert_eq!(v["required"], false);
    assert_eq!(v["value"], "");
    assert!(v["placeholder"].is_null());
    assert!(v["description"].is_null());
    assert_eq!(v["readonly"], false);
    assert_eq!(v["localized"], false);
    assert_eq!(v["locale_locked"], false);
    // Optional keys absent when None.
    assert!(v.get("position").is_none());
    assert!(v.get("error").is_none());
    assert!(v.get("min_length").is_none());
    assert!(v.get("min").is_none());
    assert!(v.get("has_min").is_none());
    assert!(v.get("condition_visible").is_none());
}

#[test]
fn base_optional_keys_emit_when_set() {
    let mut b = base("title", "text");
    b.position = Some("sidebar".to_string());
    b.error = Some("required".to_string());
    b.validation.min_length = Some(3);
    b.validation.max_length = Some(120);
    b.validation.min = Some(1.0);
    b.validation.has_min = Some(true);
    b.condition.condition_visible = Some(true);
    b.condition.condition_ref = Some("conditions.is_admin".to_string());

    let f = TextField {
        base: b,
        has_many: None,
        tags: None,
    };
    let v = serde_json::to_value(FieldContext::Text(f)).unwrap();
    assert_eq!(v["position"], "sidebar");
    assert_eq!(v["error"], "required");
    assert_eq!(v["min_length"], 3);
    assert_eq!(v["max_length"], 120);
    assert_eq!(v["min"], 1.0);
    assert_eq!(v["has_min"], true);
    assert_eq!(v["condition_visible"], true);
    assert_eq!(v["condition_ref"], "conditions.is_admin");
}

// ── Text-like variants ─────────────────────────────────────────────

#[test]
fn text_with_has_many_emits_tags() {
    let f = TextField {
        base: base("tags", "text"),
        has_many: Some(true),
        tags: Some(vec!["rust".to_string(), "cms".to_string()]),
    };
    let v = serde_json::to_value(FieldContext::Text(f)).unwrap();
    assert_eq!(v["has_many"], true);
    assert_eq!(v["tags"], json!(["rust", "cms"]));
}

#[test]
fn text_without_has_many_omits_keys() {
    let f = TextField {
        base: base("title", "text"),
        has_many: None,
        tags: None,
    };
    let v = serde_json::to_value(FieldContext::Text(f)).unwrap();
    assert!(v.get("has_many").is_none());
    assert!(v.get("tags").is_none());
}

#[test]
fn email_variant_uses_email_field_type() {
    let f = TextField {
        base: base("contact", "email"),
        has_many: None,
        tags: None,
    };
    let v = serde_json::to_value(FieldContext::Email(f)).unwrap();
    assert_eq!(v["field_type"], "email");
}

// ── Textarea ───────────────────────────────────────────────────────

#[test]
fn textarea_always_emits_rows_and_resizable() {
    let f = TextareaField {
        base: base("body", "textarea"),
        rows: 8,
        resizable: true,
    };
    let v = serde_json::to_value(FieldContext::Textarea(f)).unwrap();
    assert_eq!(v["rows"], 8);
    assert_eq!(v["resizable"], true);
}

// ── Number ─────────────────────────────────────────────────────────

#[test]
fn number_always_emits_step() {
    let f = NumberField {
        base: base("count", "number"),
        step: "any".to_string(),
        has_many: None,
        tags: None,
    };
    let v = serde_json::to_value(FieldContext::Number(f)).unwrap();
    assert_eq!(v["step"], "any");
    assert!(v.get("has_many").is_none());
}

// ── Code ───────────────────────────────────────────────────────────

#[test]
fn code_emits_language_and_optional_languages() {
    let f = CodeField {
        base: base("snippet", "code"),
        language: "javascript".to_string(),
        languages: Some(vec!["javascript".to_string(), "python".to_string()]),
    };
    let v = serde_json::to_value(FieldContext::Code(f)).unwrap();
    assert_eq!(v["language"], "javascript");
    assert_eq!(v["languages"], json!(["javascript", "python"]));
}

#[test]
fn code_without_languages_omits_picker_key() {
    let f = CodeField {
        base: base("snippet", "code"),
        language: "json".to_string(),
        languages: None,
    };
    let v = serde_json::to_value(FieldContext::Code(f)).unwrap();
    assert_eq!(v["language"], "json");
    assert!(v.get("languages").is_none());
}

// ── Richtext ───────────────────────────────────────────────────────

#[test]
fn richtext_renames_node_names_with_underscore_prefix() {
    let f = RichtextField {
        base: base("body", "richtext"),
        resizable: false,
        richtext_format: "html".to_string(),
        features: Some(vec!["bold".to_string()]),
        node_names: Some(vec!["paragraph".to_string()]),
    };
    let v = serde_json::to_value(FieldContext::Richtext(f)).unwrap();
    // Per the existing on-the-wire shape consumed by <crap-richtext>.
    assert_eq!(v["_node_names"], json!(["paragraph"]));
    assert_eq!(v["features"], json!(["bold"]));
    assert_eq!(v["richtext_format"], "html");
}

// ── Date ───────────────────────────────────────────────────────────

#[test]
fn date_day_only_sets_date_only_value() {
    let f = DateField {
        base: base("published", "date"),
        picker_appearance: "dayOnly".to_string(),
        date_only_value: Some("2026-01-15".to_string()),
        datetime_local_value: None,
        min_date: None,
        max_date: None,
        timezone_enabled: None,
        default_timezone: None,
        timezone_options: None,
        timezone_value: None,
    };
    let v = serde_json::to_value(FieldContext::Date(f)).unwrap();
    assert_eq!(v["picker_appearance"], "dayOnly");
    assert_eq!(v["date_only_value"], "2026-01-15");
    assert!(v.get("datetime_local_value").is_none());
}

#[test]
fn date_with_timezone_emits_picker_keys() {
    let f = DateField {
        base: base("published", "date"),
        picker_appearance: "dayAndTime".to_string(),
        date_only_value: None,
        datetime_local_value: Some("2026-01-15T09:30".to_string()),
        min_date: None,
        max_date: None,
        timezone_enabled: Some(true),
        default_timezone: Some("America/New_York".to_string()),
        timezone_options: Some(vec![TimezoneOption {
            value: "UTC".to_string(),
            label: "UTC".to_string(),
        }]),
        timezone_value: Some("Europe/Berlin".to_string()),
    };
    let v = serde_json::to_value(FieldContext::Date(f)).unwrap();
    assert_eq!(v["timezone_enabled"], true);
    assert_eq!(v["default_timezone"], "America/New_York");
    assert_eq!(v["timezone_value"], "Europe/Berlin");
    assert_eq!(v["timezone_options"][0]["value"], "UTC");
}

// ── Choice (Select / Radio) ────────────────────────────────────────

#[test]
fn select_emits_options_and_optional_has_many() {
    let f = ChoiceField {
        base: base("color", "select"),
        options: vec![
            SelectOption {
                label: "Red".to_string(),
                value: "red".to_string(),
                selected: false,
            },
            SelectOption {
                label: "Green".to_string(),
                value: "green".to_string(),
                selected: true,
            },
        ],
        has_many: None,
    };
    let v = serde_json::to_value(FieldContext::Select(f)).unwrap();
    let opts = v["options"].as_array().unwrap();
    assert_eq!(opts.len(), 2);
    assert_eq!(opts[0]["label"], "Red");
    assert_eq!(opts[1]["selected"], true);
    assert!(v.get("has_many").is_none());
}

// ── Checkbox ───────────────────────────────────────────────────────

#[test]
fn checkbox_emits_checked() {
    let f = CheckboxField {
        base: base("active", "checkbox"),
        checked: true,
    };
    let v = serde_json::to_value(FieldContext::Checkbox(f)).unwrap();
    assert_eq!(v["checked"], true);
}

// ── Relationship ───────────────────────────────────────────────────

#[test]
fn relationship_with_polymorphic_emits_collections() {
    let f = RelationshipField {
        base: base("author", "relationship"),
        relationship_collection: Some("users".to_string()),
        has_many: Some(false),
        polymorphic: Some(true),
        collections: Some(vec!["users".to_string(), "guests".to_string()]),
        picker: Some("drawer".to_string()),
        selected_items: None,
    };
    let v = serde_json::to_value(FieldContext::Relationship(f)).unwrap();
    assert_eq!(v["relationship_collection"], "users");
    assert_eq!(v["polymorphic"], true);
    assert_eq!(v["collections"], json!(["users", "guests"]));
}

#[test]
fn relationship_with_selected_items_flat() {
    let f = RelationshipField {
        base: base("author", "relationship"),
        relationship_collection: Some("users".to_string()),
        has_many: Some(true),
        polymorphic: None,
        collections: None,
        picker: None,
        selected_items: Some(RelationshipSelected::Flat(vec![RelationshipSelectedItem {
            id: "u1".to_string(),
            label: "Alice".to_string(),
        }])),
    };
    let v = serde_json::to_value(FieldContext::Relationship(f)).unwrap();
    let items = v["selected_items"].as_array().unwrap();
    assert_eq!(items[0]["id"], "u1");
    assert_eq!(items[0]["label"], "Alice");
    assert!(items[0].get("collection").is_none());
}

#[test]
fn relationship_polymorphic_selected_items_carry_collection() {
    let f = RelationshipField {
        base: base("ref", "relationship"),
        relationship_collection: Some("users".to_string()),
        has_many: Some(false),
        polymorphic: Some(true),
        collections: Some(vec!["users".to_string()]),
        picker: None,
        selected_items: Some(RelationshipSelected::Polymorphic(vec![
            SelectedCollectionItem {
                id: "u1".to_string(),
                label: "Alice".to_string(),
                collection: "users".to_string(),
            },
        ])),
    };
    let v = serde_json::to_value(FieldContext::Relationship(f)).unwrap();
    let items = v["selected_items"].as_array().unwrap();
    assert_eq!(items[0]["collection"], "users");
}

// ── Upload ─────────────────────────────────────────────────────────

#[test]
fn upload_with_picker_default() {
    let f = UploadField {
        base: base("image", "upload"),
        relationship_collection: Some("media".to_string()),
        has_many: None,
        picker: Some("drawer".to_string()),
        selected_items: None,
    };
    let v = serde_json::to_value(FieldContext::Upload(f)).unwrap();
    assert_eq!(v["relationship_collection"], "media");
    assert_eq!(v["picker"], "drawer");
}

// ── Join ───────────────────────────────────────────────────────────

#[test]
fn join_emits_collection_and_on() {
    let mut b = base("posts", "join");
    b.readonly = true;
    let f = JoinField {
        base: b,
        join_collection: Some("posts".to_string()),
        join_on: Some("author_id".to_string()),
    };
    let v = serde_json::to_value(FieldContext::Join(f)).unwrap();
    assert_eq!(v["readonly"], true);
    assert_eq!(v["join_collection"], "posts");
    assert_eq!(v["join_on"], "author_id");
}

// ── Group / Collapsible ────────────────────────────────────────────

#[test]
fn group_serializes_sub_fields_recursively() {
    let inner = TextField {
        base: base("title", "text"),
        has_many: None,
        tags: None,
    };
    let g = GroupField {
        base: base("meta", "group"),
        sub_fields: vec![FieldContext::Text(inner)],
        collapsed: false,
    };
    let v = serde_json::to_value(FieldContext::Group(g)).unwrap();
    let subs = v["sub_fields"].as_array().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0]["name"], "title");
    assert_eq!(subs[0]["field_type"], "text");
    assert_eq!(v["collapsed"], false);
}

#[test]
fn collapsible_uses_same_shape_as_group() {
    let g = GroupField {
        base: base("meta", "collapsible"),
        sub_fields: vec![],
        collapsed: true,
    };
    let v = serde_json::to_value(FieldContext::Collapsible(g)).unwrap();
    assert_eq!(v["field_type"], "collapsible");
    assert_eq!(v["collapsed"], true);
    assert_eq!(v["sub_fields"], json!([]));
}

// ── Row ────────────────────────────────────────────────────────────

#[test]
fn row_omits_collapsed_key() {
    let r = RowField {
        base: base("layout", "row"),
        sub_fields: vec![],
    };
    let v = serde_json::to_value(FieldContext::Row(r)).unwrap();
    assert!(v.get("collapsed").is_none());
    assert_eq!(v["sub_fields"], json!([]));
}

// ── Tabs ───────────────────────────────────────────────────────────

#[test]
fn tabs_include_panels_with_optional_error_count() {
    let t = TabsField {
        base: base("settings", "tabs"),
        tabs: vec![
            TabPanel {
                label: "General".to_string(),
                sub_fields: vec![],
                error_count: None,
                description: None,
            },
            TabPanel {
                label: "Advanced".to_string(),
                sub_fields: vec![],
                error_count: Some(2),
                description: Some("danger zone".to_string()),
            },
        ],
    };
    let v = serde_json::to_value(FieldContext::Tabs(t)).unwrap();
    let tabs = v["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[0]["label"], "General");
    assert!(tabs[0].get("error_count").is_none());
    assert_eq!(tabs[1]["error_count"], 2);
    assert_eq!(tabs[1]["description"], "danger zone");
}

// ── Array ──────────────────────────────────────────────────────────

#[test]
fn array_at_build_time_has_no_rows_and_zero_count() {
    let a = ArrayField {
        base: base("items", "array"),
        sub_fields: vec![],
        rows: None,
        row_count: 0,
        template_id: "items".to_string(),
        min_rows: Some(1),
        max_rows: Some(5),
        init_collapsed: false,
        add_label: Some("Item".to_string()),
        label_field: Some("name".to_string()),
    };
    let v = serde_json::to_value(FieldContext::Array(a)).unwrap();
    assert_eq!(v["row_count"], 0);
    assert!(v.get("rows").is_none());
    assert_eq!(v["template_id"], "items");
    assert_eq!(v["min_rows"], 1);
    assert_eq!(v["max_rows"], 5);
    assert_eq!(v["add_label"], "Item");
    assert_eq!(v["label_field"], "name");
}

#[test]
fn array_after_enrichment_has_rows() {
    let row = ArrayRow {
        index: 0,
        sub_fields: vec![],
        has_errors: None,
        custom_label: Some("First row".to_string()),
    };
    let a = ArrayField {
        base: base("items", "array"),
        sub_fields: vec![],
        rows: Some(vec![row]),
        row_count: 1,
        template_id: "items".to_string(),
        min_rows: None,
        max_rows: None,
        init_collapsed: false,
        add_label: None,
        label_field: None,
    };
    let v = serde_json::to_value(FieldContext::Array(a)).unwrap();
    assert_eq!(v["row_count"], 1);
    let rows = v["rows"].as_array().unwrap();
    assert_eq!(rows[0]["index"], 0);
    assert_eq!(rows[0]["custom_label"], "First row");
    assert!(rows[0].get("has_errors").is_none());
}

// ── Blocks ─────────────────────────────────────────────────────────

#[test]
fn blocks_emit_block_definitions() {
    let bd = BlockDefinition {
        block_type: "text".to_string(),
        label: "Text".to_string(),
        fields: vec![],
        label_field: Some("body".to_string()),
        group: None,
        image_url: None,
    };
    let b = BlocksField {
        base: base("content", "blocks"),
        block_definitions: vec![bd],
        rows: None,
        row_count: 0,
        template_id: "content".to_string(),
        min_rows: None,
        max_rows: None,
        init_collapsed: false,
        add_label: None,
        picker: Some("inline".to_string()),
    };
    let v = serde_json::to_value(FieldContext::Blocks(b)).unwrap();
    let defs = v["block_definitions"].as_array().unwrap();
    assert_eq!(defs[0]["block_type"], "text");
    assert_eq!(defs[0]["label"], "Text");
    assert_eq!(defs[0]["label_field"], "body");
    assert!(defs[0].get("group").is_none());
    assert_eq!(v["picker"], "inline");
}

#[test]
fn block_row_carries_block_type() {
    let row = BlockRow {
        index: 0,
        block_type: "hero".to_string(),
        sub_fields: vec![],
        has_errors: Some(true),
        custom_label: None,
    };
    let v = serde_json::to_value(&row).unwrap();
    assert_eq!(v["block_type"], "hero");
    assert_eq!(v["has_errors"], true);
}

// ── Untagged enum: no wrapper, flat shape ──────────────────────────

#[test]
fn untagged_enum_produces_no_variant_wrapper() {
    let f = TextField {
        base: base("title", "text"),
        has_many: None,
        tags: None,
    };
    let v = serde_json::to_value(FieldContext::Text(f)).unwrap();
    // Untagged means no `{"Text": {...}}` wrapper — the keys are at root.
    assert!(v.is_object());
    assert!(v.get("Text").is_none());
    assert_eq!(v["name"], "title");
}
