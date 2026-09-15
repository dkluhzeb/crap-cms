//! The one column encoding and read decoding of a field's value, and the typed
//! form a value takes inside a JSON-stored row.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    core::{FieldDefinition, FieldType, json_truthy},
    db::{
        DbValue,
        query::helpers::{
            coerce_date_value_json, coerce_has_many_scalar, coerce_json_value,
            parse_has_many_scalar,
        },
    },
};

/// The column value a write stores for `field` — the one encoding every write
/// path uses (create, update, version restore). A scalar has-many list becomes
/// its JSON text, a timezone date is normalized with its zone (`tz`), and any
/// other value is coerced by the field's type.
pub(crate) fn column_value(field: &FieldDefinition, value: &Value, tz: Option<&str>) -> DbValue {
    if field.is_has_many_scalar() {
        return coerce_has_many_scalar(&field.field_type, value);
    }

    if field.has_tz_companion() {
        return coerce_date_value_json(&field.field_type, value, tz);
    }

    coerce_json_value(&field.field_type, value)
}

/// The column value of a companion column — a timezone date's `_tz` zone or a
/// code field's `_lang` language: the text, or NULL when it is absent or empty.
pub(crate) fn companion_value(value: Option<&Value>) -> DbValue {
    value
        .and_then(Value::as_str)
        .filter(|zone| !zone.is_empty())
        .map_or(DbValue::Null, |zone| DbValue::Text(zone.to_string()))
}

/// The companion columns a write of `field`'s column `base` stores from `data`,
/// each with its value — the sent text, or NULL. A companion bound to the value
/// (a date's zone) is written whenever the value is sent; any other (a code
/// field's language) only when its own key is, so an absent language keeps the
/// stored pick. The one rule the document and array-row writers share.
pub(in crate::db::query) fn companion_writes(
    field: &FieldDefinition,
    base: &str,
    data: &HashMap<String, Value>,
) -> Vec<(String, DbValue)> {
    let value_sent = data.contains_key(base);

    field
        .written_companion_columns(base, value_sent, |column| data.contains_key(column))
        .map(|column| {
            let value = companion_value(data.get(&column));
            (column, value)
        })
        .collect()
}

/// Whether reads decode `field`'s column from its JSON form — true for a
/// scalar has-many list, stored as JSON text.
pub(crate) fn decodes(field: &FieldDefinition) -> bool {
    field.is_has_many_scalar()
}

/// A stored column's JSON value as reads return it — the one decoding of a
/// field's column: a scalar has-many list parsed from its JSON text, anything
/// else as the column's JSON form holds it.
pub(crate) fn decode_value(field: &FieldDefinition, value: &Value) -> Value {
    if decodes(field) {
        return parse_has_many_scalar(&field.field_type, value);
    }

    value.clone()
}

/// The value a write of `value` stores, as a read returns it: encoded to its
/// column value and decoded back.
pub(crate) fn stored_value(field: &FieldDefinition, value: &Value, tz: Option<&str>) -> Value {
    decode_value(field, &column_value(field, value, tz).to_json())
}

/// The value a write stores inside a JSON-stored row — a blocks row, a group or
/// array nested in a row — as reads return it: the one form such a value takes
/// whoever wrote it. A checkbox is `true`/`false`, a blank value null, and a
/// number, date, text, email or scalar has-many list is what its column would
/// hold. Any other value — JSON, rich text, a reference — stays as sent, and so
/// does a value that doesn't encode (a number field holding text), which is kept
/// rather than dropped.
pub(crate) fn nested_value(field: &FieldDefinition, value: &Value, tz: Option<&str>) -> Value {
    if field.field_type == FieldType::Checkbox {
        return Value::Bool(json_truthy(value));
    }

    if value.as_str().is_some_and(str::is_empty) {
        return Value::Null;
    }

    if value.is_null() || !nests_typed(field) {
        return value.clone();
    }

    let stored = stored_value(field, value, tz);

    if stored.is_null() {
        value.clone()
    } else {
        stored
    }
}

/// Whether a value of `field` inside a JSON-stored row takes the form its column
/// would hold: a number, date, text, email or scalar has-many list.
fn nests_typed(field: &FieldDefinition) -> bool {
    field.is_has_many_scalar()
        || matches!(
            field.field_type,
            FieldType::Number
                | FieldType::Date
                | FieldType::Text
                | FieldType::Textarea
                | FieldType::Email
        )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::FieldAdmin;

    fn code_field() -> FieldDefinition {
        FieldDefinition::builder("snippet", FieldType::Code)
            .admin(
                FieldAdmin::builder()
                    .languages(vec!["python".to_string()])
                    .build(),
            )
            .build()
    }

    /// A write stores a date's zone whenever the date is sent — as NULL when no
    /// zone came with it — and a code field's language pick only when its own
    /// key is sent, so an absent pick keeps the stored one. A blank pick is NULL.
    #[test]
    fn a_companion_write_follows_its_write_policy() {
        let date = FieldDefinition::builder("starts", FieldType::Date)
            .timezone(true)
            .build();
        let code = code_field();

        let value_only = HashMap::from([
            ("starts".to_string(), json!("2026-01-01T09:00")),
            ("snippet".to_string(), json!("print(1)")),
        ]);

        assert_eq!(
            companion_writes(&date, "starts", &value_only),
            vec![("starts_tz".to_string(), DbValue::Null)]
        );
        assert!(
            companion_writes(&code, "snippet", &value_only).is_empty(),
            "an absent language pick keeps the stored one"
        );

        let blank_pick = HashMap::from([("snippet_lang".to_string(), json!(""))]);

        assert_eq!(
            companion_writes(&code, "snippet", &blank_pick),
            vec![("snippet_lang".to_string(), DbValue::Null)]
        );
    }

    /// A written value reads back in the form its column holds: a checkbox as
    /// `0`/`1`, JSON as its text, a timezone date normalized, a list typed.
    #[test]
    fn a_stored_value_reads_as_its_column_holds_it() {
        let checkbox = FieldDefinition::builder("done", FieldType::Checkbox).build();
        let json_field = FieldDefinition::builder("meta", FieldType::Json).build();
        let list = FieldDefinition::builder("scores", FieldType::Number)
            .has_many(true)
            .build();
        let date = FieldDefinition::builder("starts", FieldType::Date)
            .timezone(true)
            .build();

        assert_eq!(stored_value(&checkbox, &json!(true), None), json!(1));
        assert_eq!(
            stored_value(&json_field, &json!({ "n": 1 }), None),
            json!("{\"n\":1}")
        );
        assert_eq!(stored_value(&list, &json!(["1", 2.0]), None), json!([1, 2]));
        assert_eq!(
            stored_value(&date, &json!("2026-01-01T09:00"), Some("Europe/Berlin")),
            json!("2026-01-01T08:00:00.000Z")
        );
    }

    /// A checkbox value is checked or not alike whether its write lands in a
    /// column or inside a JSON-stored row.
    #[test]
    fn a_checkbox_column_and_nested_value_agree() {
        let checkbox = FieldDefinition::builder("done", FieldType::Checkbox).build();

        for value in [
            json!(true),
            json!(false),
            json!(2),
            json!(1.0),
            json!(0.5),
            json!(0),
            json!(-1),
            json!("on"),
            json!("off"),
        ] {
            let checked = nested_value(&checkbox, &value, None) == Value::Bool(true);

            assert_eq!(
                column_value(&checkbox, &value, None),
                DbValue::Integer(i64::from(checked)),
                "{value}"
            );
        }
    }
}
