//! The one data view every server-side display-condition evaluation sees.
//!
//! A condition judges the form's values, and the same form reaches the server
//! in different shapes: the stored document on the edit render, the field
//! defaults on the create render, the submitted strings on an error
//! re-render, and the browser's live snapshot at the evaluate endpoint. Every
//! one of them is decoded here the way the write path stores it — group
//! values nested under their group (`seo.title`), a checkbox as `true` /
//! `false`, a number as a number, an empty input as `null`, a `has_many`
//! value as a list — so a condition sees one shape wherever it runs. The
//! browser evaluator (`static/components/conditions.js`) decodes its live
//! snapshot into the same shape.

use serde_json::{Map, Value};

use crate::{
    admin::handlers::{forms::FormData, validate::values_to_string_map},
    core::{DocumentFields, FieldDefinition, nest_group_fields, prefixed_name, walk_leaf_fields},
    db::query::join::store_nested_values,
};

/// Decode `data` — flat form keys (`seo__title`) or nested objects, raw form
/// strings or stored values — into the condition data view.
#[must_use]
pub fn condition_data(fields: &[FieldDefinition], data: &DocumentFields) -> Value {
    let nested = nest_group_fields(data, fields);
    let mut level: Map<String, Value> = nested.into_inner().into_iter().collect();

    store_nested_values(&mut level, fields);

    Value::Object(level)
}

/// The condition data of a submitted form, decoded like its write.
#[must_use]
pub fn form_condition_data(fields: &[FieldDefinition], form: &FormData) -> Value {
    condition_data(fields, &form.to_doc_fields())
}

/// The condition data of the browser's live form snapshot (the evaluate
/// endpoint's payload — the same name → value map the validate endpoint
/// receives), parsed exactly as a submission of that form is.
#[must_use]
pub fn live_condition_data(fields: &[FieldDefinition], snapshot: &DocumentFields) -> Value {
    let form = FormData::from_raw(values_to_string_map(snapshot), fields);

    form_condition_data(fields, &form)
}

/// The condition data of a new document: every field at its `default_value`,
/// the values the create form renders its inputs with.
#[must_use]
pub fn default_condition_data(fields: &[FieldDefinition]) -> Value {
    let mut defaults = DocumentFields::new();

    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, _| {
        if let Some(value) = &field.default_value {
            defaults.insert(prefixed_name(prefix, &field.name), value.clone());
        }

        Ok(())
    });

    condition_data(fields, &defaults)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::core::{ConditionExpr, FieldType, PickerAppearance};

    fn schema() -> Vec<FieldDefinition> {
        let mut tags = FieldDefinition::builder("tags", FieldType::Select).build();
        tags.has_many = true;

        vec![
            FieldDefinition::builder("online", FieldType::Checkbox).build(),
            FieldDefinition::builder("seats", FieldType::Number).build(),
            FieldDefinition::builder("kind", FieldType::Select).build(),
            FieldDefinition::builder("note", FieldType::Text).build(),
            tags,
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                ])
                .build(),
        ]
    }

    fn form(pairs: &[(&str, &str)]) -> FormData {
        let raw: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();

        FormData::from_raw(raw, &schema())
    }

    fn holds(condition: Value, data: &Value) -> bool {
        let expr: ConditionExpr = serde_json::from_value(condition).unwrap();

        expr.evaluate(data)
    }

    /// Regression: an error re-render conditioned on the raw strings the form
    /// posted (`"on"`, `"5"`, `seo__title`) while the edit render conditioned
    /// on the stored document (`true`, `5`, `seo.title`), so one condition
    /// showed a field on one render and hid it on the other.
    #[test]
    fn a_submitted_form_decodes_like_the_stored_document() {
        let submitted = form_condition_data(
            &schema(),
            &form(&[
                ("online", "on"),
                ("seats", "5"),
                ("kind", "talk"),
                ("note", ""),
                ("tags", "a"),
                ("seo__title", "Hi"),
            ]),
        );

        let stored: DocumentFields = [
            ("online".to_string(), json!(true)),
            ("seats".to_string(), json!(5)),
            ("kind".to_string(), json!("talk")),
            ("note".to_string(), Value::Null),
            ("tags".to_string(), json!(["a"])),
            ("seo".to_string(), json!({ "title": "Hi" })),
        ]
        .into_iter()
        .collect();

        assert_eq!(submitted, condition_data(&schema(), &stored));
    }

    /// A date decodes to its stored UTC instant on every path — the browser
    /// evaluator (`static/components/_internal/stored-date.js`) mirrors it:
    /// a day is noon UTC, a date and time with a chosen zone is converted
    /// from that zone.
    #[test]
    fn a_submitted_date_decodes_like_the_stored_date() {
        let fields = vec![
            FieldDefinition::builder("day", FieldType::Date).build(),
            FieldDefinition::builder("meet", FieldType::Date)
                .picker_appearance(PickerAppearance::DayAndTime)
                .timezone(true)
                .build(),
        ];
        let raw: HashMap<String, String> = [
            ("day", "2026-01-15"),
            ("meet", "2026-07-15T09:00"),
            ("meet_tz", "Europe/Berlin"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let submitted = form_condition_data(&fields, &FormData::from_raw(raw, &fields));

        let stored: DocumentFields = [
            ("day".to_string(), json!("2026-01-15T12:00:00.000Z")),
            ("meet".to_string(), json!("2026-07-15T07:00:00.000Z")),
            ("meet_tz".to_string(), json!("Europe/Berlin")),
        ]
        .into_iter()
        .collect();

        assert_eq!(submitted, condition_data(&fields, &stored));
        assert_eq!(submitted["meet"], json!("2026-07-15T07:00:00.000Z"));
    }

    /// The shipped example's `{ field = "online", equals = true }`: an
    /// unchecked box posts nothing, and both states must decode to a boolean.
    #[test]
    fn a_checkbox_condition_holds_on_both_states() {
        let checked = form_condition_data(&schema(), &form(&[("online", "on")]));
        let unchecked = form_condition_data(&schema(), &form(&[]));
        let condition = json!({ "field": "online", "equals": true });

        assert!(holds(condition.clone(), &checked));
        assert!(!holds(condition, &unchecked));
    }

    #[test]
    fn a_number_and_a_group_sub_field_condition_hold() {
        let data = form_condition_data(&schema(), &form(&[("seats", "5"), ("seo__title", "Hi")]));

        assert!(holds(json!({ "field": "seats", "equals": 5 }), &data));
        assert!(holds(
            json!({ "field": "seo.title", "equals": "Hi" }),
            &data
        ));
        assert!(holds(
            json!({ "field": "seo__title", "equals": "Hi" }),
            &data
        ));
    }

    /// The live endpoint receives the browser's name → value snapshot (a
    /// repeated name as a list) and decodes it like the submission it mirrors.
    #[test]
    fn the_live_snapshot_decodes_like_a_submission() {
        let snapshot: DocumentFields = [
            ("online".to_string(), json!("on")),
            ("tags".to_string(), json!(["a", "b"])),
        ]
        .into_iter()
        .collect();

        let data = live_condition_data(&schema(), &snapshot);

        assert_eq!(data["online"], json!(true));
        assert_eq!(data["tags"], json!(["a", "b"]));
    }

    /// Regression: the create form conditioned on `{}`, so a field shown by a
    /// select's default value rendered hidden until the user re-selected it.
    #[test]
    fn a_new_document_conditions_on_its_defaults() {
        let mut fields = schema();
        fields[2].default_value = Some(json!("link"));
        fields[0].default_value = Some(json!(true));

        let data = default_condition_data(&fields);

        assert!(holds(json!({ "field": "kind", "equals": "link" }), &data));
        assert!(holds(json!({ "field": "online", "equals": true }), &data));
    }
}
