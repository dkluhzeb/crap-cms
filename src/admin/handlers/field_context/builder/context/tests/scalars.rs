//! Scalar fields: dates, relationships, checkboxes, uploads, selects and rich text.

use std::collections::HashMap;

use serde_json::json;

use crate::{
    admin::handlers::field_context::test_helpers::{build_value_contexts, make_field},
    core::{
        FieldDefinition, FieldType, LocalizedString, PickerAppearance, RelationshipConfig,
        SelectOption as CoreSelectOption,
    },
};

// ── Date ──────────────────────────────────────────────────────────

#[test]
fn build_field_contexts_date_default_day_only() {
    let date_field = make_field("published_at", FieldType::Date);
    let fields = vec![date_field];
    let mut values = HashMap::new();
    values.insert(
        "published_at".to_string(),
        "2026-01-15T12:00:00.000Z".to_string(),
    );
    let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
    assert_eq!(result[0]["picker_appearance"], "dayOnly");
    assert_eq!(result[0]["date_only_value"], "2026-01-15");
}

#[test]
fn build_field_contexts_date_day_and_time() {
    let mut date_field = make_field("event_at", FieldType::Date);
    date_field.picker_appearance = Some(PickerAppearance::DayAndTime);
    let fields = vec![date_field];
    let mut values = HashMap::new();
    values.insert(
        "event_at".to_string(),
        "2026-01-15T09:30:00.000Z".to_string(),
    );
    let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
    assert_eq!(result[0]["picker_appearance"], "dayAndTime");
    assert_eq!(result[0]["datetime_local_value"], "2026-01-15T09:30");
}

#[test]
fn build_field_contexts_date_time_only() {
    let mut date_field = make_field("reminder", FieldType::Date);
    date_field.picker_appearance = Some(PickerAppearance::TimeOnly);
    let fields = vec![date_field];
    let mut values = HashMap::new();
    values.insert("reminder".to_string(), "14:30".to_string());
    let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
    assert_eq!(result[0]["picker_appearance"], "timeOnly");
    assert_eq!(result[0]["value"], "14:30");
}

#[test]
fn build_field_contexts_date_month_only() {
    let mut date_field = make_field("birth_month", FieldType::Date);
    date_field.picker_appearance = Some(PickerAppearance::MonthOnly);
    let fields = vec![date_field];
    let mut values = HashMap::new();
    values.insert("birth_month".to_string(), "2026-01".to_string());
    let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
    assert_eq!(result[0]["picker_appearance"], "monthOnly");
    assert_eq!(result[0]["value"], "2026-01");
}

#[test]
fn build_field_contexts_date_short_value_day_only() {
    let mut values = HashMap::new();
    values.insert("d".to_string(), "short".to_string()); // less than 10 chars
    let field = make_field("d", FieldType::Date);
    let fields = vec![field];
    let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
    // Should use the short value as-is.
    assert_eq!(result[0]["date_only_value"], "short");
}

#[test]
fn build_field_contexts_date_short_value_day_and_time() {
    let mut field = make_field("d", FieldType::Date);
    field.picker_appearance = Some(PickerAppearance::DayAndTime);
    let mut values = HashMap::new();
    values.insert("d".to_string(), "short".to_string()); // less than 16 chars
    let fields = vec![field];
    let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
    assert_eq!(result[0]["datetime_local_value"], "short");
}

// ── Relationship ──────────────────────────────────────────────────

#[test]
fn build_field_contexts_relationship_has_collection_info() {
    let mut rel_field = make_field("author", FieldType::Relationship);
    rel_field.relationship = Some(RelationshipConfig::new("users", false));
    let fields = vec![rel_field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["relationship_collection"], "users");
    assert_eq!(result[0]["has_many"], false);
}

#[test]
fn build_field_contexts_relationship_has_many() {
    let mut rel_field = make_field("tags", FieldType::Relationship);
    rel_field.relationship = Some(RelationshipConfig::new("tags", true));
    let fields = vec![rel_field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["relationship_collection"], "tags");
    assert_eq!(result[0]["has_many"], true);
}

// ── Checkbox ──────────────────────────────────────────────────────

#[test]
fn build_field_contexts_checkbox_checked_values() {
    for val in &["1", "true", "on", "yes"] {
        let mut values = HashMap::new();
        values.insert("active".to_string(), val.to_string());
        let fields = vec![make_field("active", FieldType::Checkbox)];
        let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
        assert_eq!(
            result[0]["checked"], true,
            "Checkbox should be checked for value '{val}'"
        );
    }
}

#[test]
fn build_field_contexts_checkbox_unchecked_values() {
    for val in &["0", "false", "off", "no", ""] {
        let mut values = HashMap::new();
        values.insert("active".to_string(), val.to_string());
        let fields = vec![make_field("active", FieldType::Checkbox)];
        let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
        assert_eq!(
            result[0]["checked"], false,
            "Checkbox should be unchecked for value '{val}'"
        );
    }
}

/// Regression: on a new-item form (no submitted/stored value) a checkbox
/// with `default_value = true` renders CHECKED; a present stored value takes
/// precedence over the default.
#[test]
fn build_field_contexts_checkbox_true_default_checks_on_empty() {
    let fields = vec![
        FieldDefinition::builder("featured", FieldType::Checkbox)
            .default_value(json!(true))
            .build(),
    ];

    // New form: no value → default applies → checked.
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(
        result[0]["checked"], true,
        "default_value=true renders checked when there is no value"
    );

    // Present stored value overrides the default.
    let mut values = HashMap::new();
    values.insert("featured".to_string(), "0".to_string());
    let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
    assert_eq!(
        result[0]["checked"], false,
        "a present stored value takes precedence over the default"
    );
}

// ── Upload ────────────────────────────────────────────────────────

#[test]
fn build_field_contexts_upload_has_collection() {
    let mut upload_field = make_field("image", FieldType::Upload);
    upload_field.relationship = Some(RelationshipConfig::new("media", false));
    let fields = vec![upload_field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["relationship_collection"], "media");
    assert_eq!(
        result[0]["picker"], "drawer",
        "upload fields default to drawer picker"
    );
}

// ── Select ────────────────────────────────────────────────────────

#[test]
fn build_field_contexts_select_marks_selected_option() {
    let mut sel = make_field("color", FieldType::Select);
    sel.options = vec![
        CoreSelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
        CoreSelectOption::new(LocalizedString::Plain("Blue".to_string()), "blue"),
    ];
    let mut values = HashMap::new();
    values.insert("color".to_string(), "blue".to_string());
    let fields = vec![sel];
    let result = build_value_contexts(&fields, &values, &HashMap::new(), false, false);
    let opts = result[0]["options"].as_array().unwrap();
    assert_eq!(opts[0]["selected"], false);
    assert_eq!(opts[1]["selected"], true);
}

// ── richtext_format ──────────────────────────────────────────────

#[test]
fn richtext_format_defaults_to_html() {
    let field = make_field("body", FieldType::Richtext);
    let fields = vec![field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["richtext_format"], "html");
}

#[test]
fn richtext_format_json() {
    let mut field = make_field("body", FieldType::Richtext);
    field.admin.richtext_format = Some("json".to_string());
    let fields = vec![field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert_eq!(result[0]["richtext_format"], "json");
}

// ── Richtext node attr error display ──────────────────────────────

#[test]
fn build_richtext_field_shows_node_attr_errors() {
    let field = make_field("content", FieldType::Richtext);
    let fields = vec![field];
    let values = HashMap::new();
    let mut errors = HashMap::new();
    errors.insert(
        "content[cta#0].text".to_string(),
        "Text is required".to_string(),
    );

    let result = build_value_contexts(&fields, &values, &errors, false, false);
    assert_eq!(result[0]["field_type"], "richtext");
    assert_eq!(result[0]["error"], "Text is required");
}

#[test]
fn build_richtext_field_direct_error_takes_priority() {
    let field = make_field("content", FieldType::Richtext);
    let fields = vec![field];
    let values = HashMap::new();
    let mut errors = HashMap::new();
    errors.insert("content".to_string(), "Field is required".to_string());
    errors.insert(
        "content[cta#0].text".to_string(),
        "Text is required".to_string(),
    );

    let result = build_value_contexts(&fields, &values, &errors, false, false);
    assert_eq!(result[0]["error"], "Field is required");
}

#[test]
fn build_richtext_field_no_errors_no_error_key() {
    let field = make_field("content", FieldType::Richtext);
    let fields = vec![field];
    let result = build_value_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
    assert!(result[0].get("error").is_none() || result[0]["error"].is_null());
}
