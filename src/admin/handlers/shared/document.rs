//! Document helpers — value flattening, labels, validation translation, ref counts.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::{
    admin::Translations,
    core::{
        DocumentFields, FieldAdmin, FieldDefinition, FieldType, ValidationError, field, find_field,
        prefixed_name,
    },
    db::DbPool,
    hooks::HookRunner,
    service::{ServiceContext, document_info::get_ref_count},
};

/// Auto-generate a label from a field name (e.g. "`my_field`" -> "My Field").
pub fn auto_label_from_name(name: &str) -> String {
    field::to_title_case(name)
}

/// Compute a custom row label for an array or blocks row.
///
/// Priority: `row_label` Lua function > block-level `label_field` > field-level `label_field` > None.
pub fn compute_row_label(
    admin: &FieldAdmin,
    block_label_field: Option<&str>,
    row_data: Option<&Map<String, Value>>,
    hook_runner: &HookRunner,
) -> Option<String> {
    if let Some(ref func_ref) = admin.row_label
        && let Some(row) = row_data
    {
        let json_val = Value::Object(row.clone());

        if let Some(label) = hook_runner.call_row_label(func_ref, &json_val)
            && !label.is_empty()
        {
            return Some(label);
        }
    }

    let lf = block_label_field.or(admin.label_field.as_deref())?;
    let row = row_data?;
    let val = row.get(lf)?;

    let s = match val {
        Value::String(s) if !s.is_empty() => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => return None,
    };

    Some(s)
}

/// Flattens document fields for form rendering. Group fields become `parent__child` keys,
/// recursively flattening nested groups (e.g. `address: { geo: { lat: "40" } }` →
/// `address__geo__lat: "40"`).
///
/// Tabs / Row / Collapsible are transparent layout wrappers — their inner
/// fields are still at the table's top level, so Groups nested inside any
/// of them are recognised the same way as top-level Groups. Without this
/// pass-through, a `Group("social")` nested in a `Tabs` would be
/// stringified as JSON instead of expanded into `social__github`,
/// `social__twitter`, … and the form would render every social field
/// empty even though the DB has the values.
pub fn flatten_document_values(
    fields: &DocumentFields,
    field_defs: &[FieldDefinition],
) -> HashMap<String, String> {
    fields
        .iter()
        .flat_map(|(k, v)| {
            if let Value::Object(obj) = v
                && is_group_field(field_defs, k)
            {
                let mut out = Vec::new();
                flatten_group_value(k, obj, &mut out);
                return out;
            }

            vec![(k.clone(), value_to_form_string(v))]
        })
        .collect()
}

/// True iff `name` refers to a Group field at the effective top level of
/// `field_defs`. Descends Tabs/Row/Collapsible — those wrappers are
/// transparent for column-naming purposes, so a Group nested in any of
/// them is still a "top-level Group" for the form-flatten contract.
fn is_group_field(field_defs: &[FieldDefinition], name: &str) -> bool {
    find_field(name, field_defs).is_some_and(|f| f.field_type == FieldType::Group)
}

/// Recursively flatten a group object into `prefix__key` pairs.
fn flatten_group_value(prefix: &str, obj: &Map<String, Value>, out: &mut Vec<(String, String)>) {
    for (sub_k, sub_v) in obj {
        let col = prefixed_name(prefix, sub_k);

        if let Value::Object(nested) = sub_v {
            flatten_group_value(&col, nested, out);
        } else {
            out.push((col, value_to_form_string(sub_v)));
        }
    }
}

/// Convert a `serde_json` Value to a string suitable for form rendering.
fn value_to_form_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Translate validation errors using the translation system.
/// If a `FieldError` has a `key`, resolve it through `Translations::get_interpolated`;
/// otherwise use the raw English `message` (custom Lua validator messages).
pub fn translate_validation_errors(
    ve: &ValidationError,
    translations: &Translations,
    locale: &str,
) -> HashMap<String, String> {
    ve.errors
        .iter()
        .map(|e| {
            let msg = if let Some(ref key) = e.key {
                translations.get_interpolated(locale, key, &e.params)
            } else {
                e.message.clone()
            };
            (e.field.clone(), msg)
        })
        .collect()
}

/// O(1) ref count lookup for delete protection UI.
/// Returns 0 on DB errors (fail-open for display only — actual delete protection
/// is enforced by the DELETE handler).
pub fn lookup_ref_count(pool: &DbPool, slug: &str, id: &str) -> i64 {
    pool.get().ok().map_or(0, |conn| {
        let ctx = ServiceContext::slug_only(slug).conn(&conn).build();
        get_ref_count(&ctx, id).unwrap_or(0)
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::core::{FieldAdmin, FieldDefinition, FieldType, validate::FieldError};

    use super::*;

    #[test]
    fn auto_label_underscore_separated() {
        assert_eq!(auto_label_from_name("my_field"), "My Field");
    }

    #[test]
    fn auto_label_single_word() {
        assert_eq!(auto_label_from_name("title"), "Title");
    }

    #[test]
    fn auto_label_empty_string() {
        assert_eq!(auto_label_from_name(""), "");
    }

    #[test]
    fn auto_label_multiple_words() {
        assert_eq!(auto_label_from_name("created_at"), "Created At");
    }

    #[test]
    fn auto_label_double_underscore() {
        assert_eq!(auto_label_from_name("seo__title"), "Seo Title");
    }

    #[test]
    fn compute_row_label_from_label_field() {
        let admin = FieldAdmin::builder().label_field("title").build();
        let mut row = Map::new();
        row.insert("title".to_string(), json!("My Title"));

        let lf = admin.label_field.as_deref();
        assert_eq!(lf, Some("title"));

        let val = row.get("title").unwrap();
        match val {
            Value::String(s) if !s.is_empty() => assert_eq!(s, "My Title"),
            _ => panic!("Expected non-empty string"),
        }
    }

    #[test]
    fn compute_row_label_number_value() {
        let val = json!(42);
        match &val {
            Value::Number(n) => assert_eq!(n.to_string(), "42"),
            _ => panic!("Expected number"),
        }
    }

    #[test]
    fn compute_row_label_bool_value() {
        let val = json!(true);
        match &val {
            Value::Bool(b) => assert_eq!(b.to_string(), "true"),
            _ => panic!("Expected bool"),
        }
    }

    #[test]
    fn flatten_document_values_simple_fields() {
        let mut fields = DocumentFields::new();
        fields.insert("title".to_string(), json!("Hello"));
        fields.insert("count".to_string(), json!(42));

        let defs = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("count", FieldType::Number).build(),
        ];

        let flat = flatten_document_values(&fields, &defs);
        assert_eq!(flat.get("title").unwrap(), "Hello");
        assert_eq!(flat.get("count").unwrap(), "42");
    }

    #[test]
    fn flatten_document_values_group_fields() {
        let mut fields = DocumentFields::new();
        fields.insert(
            "config".to_string(),
            json!({"label": "My Config", "enabled": true}),
        );

        let defs = vec![
            FieldDefinition::builder("config", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                    FieldDefinition::builder("enabled", FieldType::Checkbox).build(),
                ])
                .build(),
        ];

        let flat = flatten_document_values(&fields, &defs);
        assert_eq!(flat.get("config__label").unwrap(), "My Config");
        assert_eq!(flat.get("config__enabled").unwrap(), "true");
        assert!(!flat.contains_key("config"));
    }

    #[test]
    fn flatten_document_values_nested_groups() {
        let mut fields = DocumentFields::new();
        fields.insert("outer".to_string(), json!({"inner": {"deep": "value"}}));

        let defs = vec![
            FieldDefinition::builder("outer", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("inner", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("deep", FieldType::Text).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];

        let flat = flatten_document_values(&fields, &defs);
        assert_eq!(flat.get("outer__inner__deep").unwrap(), "value");
        assert!(!flat.contains_key("outer"));
        assert!(!flat.contains_key("outer__inner"));
    }

    #[test]
    fn flatten_document_values_group_with_array_value() {
        let mut fields = DocumentFields::new();
        fields.insert(
            "meta".to_string(),
            json!({"title": "Test", "tags": ["a", "b"]}),
        );

        let defs = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                    FieldDefinition::builder("tags", FieldType::Text).build(),
                ])
                .build(),
        ];

        let flat = flatten_document_values(&fields, &defs);
        assert_eq!(flat.get("meta__title").unwrap(), "Test");
        assert_eq!(flat.get("meta__tags").unwrap(), "[\"a\",\"b\"]");
    }

    /// Regression: a `Group` nested inside a `Tabs` wrapper (the shape
    /// `example/globals/site_settings.lua` uses for the Social tab) must
    /// still flatten to `parent__child` form keys. Before the fix,
    /// `flatten_document_values` only inspected top-level defs, so the
    /// Group was treated as an opaque object and stringified to JSON
    /// — the form rendered every social URL empty even when the DB
    /// had values.
    #[test]
    fn flatten_document_values_group_inside_tabs() {
        use crate::core::FieldTab;

        let mut fields = DocumentFields::new();
        fields.insert(
            "social".to_string(),
            json!({"github": "https://github.com/crap", "twitter": "https://test.at"}),
        );

        let defs = vec![
            FieldDefinition::builder("settings_tabs", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Social",
                    vec![
                        FieldDefinition::builder("social", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("github", FieldType::Text).build(),
                                FieldDefinition::builder("twitter", FieldType::Text).build(),
                            ])
                            .build(),
                    ],
                )])
                .build(),
        ];

        let flat = flatten_document_values(&fields, &defs);
        assert_eq!(
            flat.get("social__github").map(String::as_str),
            Some("https://github.com/crap"),
            "Group inside Tabs must flatten to __ keys"
        );
        assert_eq!(
            flat.get("social__twitter").map(String::as_str),
            Some("https://test.at")
        );
        assert!(
            !flat.contains_key("social"),
            "raw Group key must not survive the flatten when it's nested in Tabs"
        );
    }

    /// Same regression shape but with a `Row` layout wrapper. Row and
    /// Collapsible are transparent: a Group inside them lives at the
    /// table's top level just like Tabs.
    #[test]
    fn flatten_document_values_group_inside_row() {
        let mut fields = DocumentFields::new();
        fields.insert("seo".to_string(), json!({"title": "Hello"}));

        let defs = vec![
            FieldDefinition::builder("row_layout", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("seo", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("title", FieldType::Text).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];

        let flat = flatten_document_values(&fields, &defs);
        assert_eq!(flat.get("seo__title").map(String::as_str), Some("Hello"));
        assert!(!flat.contains_key("seo"));
    }

    fn test_translations() -> Translations {
        Translations::load(std::path::Path::new("/nonexistent"))
    }

    #[test]
    fn translate_with_key_uses_translation() {
        let translations = test_translations();

        let ve = ValidationError::new(vec![
            FieldError::with_key("title", "title is required", "validation.required")
                .with_param("field", "Title"),
        ]);

        let map = translate_validation_errors(&ve, &translations, "en");
        assert_eq!(map.get("title").unwrap(), "Title is required");
    }

    #[test]
    fn translate_without_key_uses_raw_message() {
        let translations = test_translations();
        let ve = ValidationError::new(vec![FieldError::new("title", "custom lua error")]);
        let map = translate_validation_errors(&ve, &translations, "en");
        assert_eq!(map.get("title").unwrap(), "custom lua error");
    }

    #[test]
    fn translate_german_locale() {
        let translations = test_translations();

        let ve = ValidationError::new(vec![
            FieldError::with_key("title", "title is required", "validation.required")
                .with_param("field", "Titel"),
        ]);

        let map = translate_validation_errors(&ve, &translations, "de");
        assert_eq!(map.get("title").unwrap(), "Titel ist erforderlich");
    }
}
