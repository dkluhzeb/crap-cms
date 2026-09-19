//! The one column encoding and read decoding of a field's value, and the typed
//! form a value takes inside a JSON-stored row.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    core::{FieldDefinition, FieldType, is_empty_object, json_truthy},
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
/// other value is coerced by the field's type. An empty has-many list spelled
/// as an empty object (a Lua table with no entries) stores as an empty list.
pub(crate) fn column_value(field: &FieldDefinition, value: &Value, tz: Option<&str>) -> DbValue {
    if field.is_list() && is_empty_object(value) {
        return column_value(field, &Value::Array(Vec::new()), tz);
    }

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

/// Whether reads decode `field`'s column rather than returning it as the column
/// holds it: a checkbox reads as a boolean, and JSON text — a scalar has-many
/// list, a JSON or JSON-format rich text value, the id list of a has-many
/// reference inside a row — reads parsed.
pub(crate) fn decodes(field: &FieldDefinition) -> bool {
    field.field_type == FieldType::Checkbox || parses_json_text(field)
}

/// Whether `field`'s column holds JSON text that reads parse.
fn parses_json_text(field: &FieldDefinition) -> bool {
    field.is_has_many_scalar() || field.parses_json() || field.is_has_many_reference()
}

/// A stored column's JSON value as reads return it — the one decoding of a
/// field's column, wherever the column lives (a table, a group's prefixed
/// column, an array row) and whatever holds it (a row, a snapshot): a checkbox
/// as `true`/`false` (an unset one as `false`), a scalar has-many list parsed
/// and typed from its JSON text, any other JSON text parsed — a text that isn't
/// JSON is kept as it is — and anything else as the column holds it. Decoding
/// a decoded value changes nothing.
pub(crate) fn decode_value(field: &FieldDefinition, value: &Value) -> Value {
    if field.field_type == FieldType::Checkbox {
        return Value::Bool(json_truthy(value));
    }

    if field.is_has_many_scalar() {
        return parse_has_many_scalar(&field.field_type, value);
    }

    if parses_json_text(field) {
        return parse_json_text(value);
    }

    value.clone()
}

/// The value a JSON text spells; any other value, or a text that isn't JSON,
/// as it is.
fn parse_json_text(value: &Value) -> Value {
    let Value::String(text) = value else {
        return value.clone();
    };

    serde_json::from_str(text).unwrap_or_else(|_| value.clone())
}

/// The value a write of `value` stores, as a read returns it: encoded to its
/// column value and decoded back.
pub(crate) fn stored_value(field: &FieldDefinition, value: &Value, tz: Option<&str>) -> Value {
    decode_value(field, &column_value(field, value, tz).to_json())
}

/// The value a write stores inside a JSON-stored row — a blocks row, a group or
/// array nested in a row — as reads return it: the one form such a value takes
/// whoever wrote it. A checkbox is `true`/`false`, a blank value null, a JSON
/// value (a JSON field, JSON-format rich text) the value its text spells, and a
/// number, date, text, email or scalar has-many list is what its column would
/// hold. An empty has-many list spelled as an empty object is an empty list.
/// Any other value — HTML rich text, a reference — stays as sent, and so does a
/// value that doesn't encode (a number field holding text), which is kept
/// rather than dropped.
pub(crate) fn nested_value(field: &FieldDefinition, value: &Value, tz: Option<&str>) -> Value {
    if field.field_type == FieldType::Checkbox {
        return Value::Bool(json_truthy(value));
    }

    if field.is_list() && is_empty_object(value) {
        return Value::Array(Vec::new());
    }

    if value.as_str().is_some_and(str::is_empty) {
        return Value::Null;
    }

    if field.parses_json() {
        return parse_json_text(value);
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
    use crate::core::{FieldAdmin, RelationshipConfig};

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

    /// A written value reads back decoded from the form its column holds: a
    /// checkbox as `true`/`false`, JSON as the value its text spells, a timezone
    /// date normalized, a list typed.
    #[test]
    fn a_stored_value_reads_decoded_from_its_column() {
        let checkbox = FieldDefinition::builder("done", FieldType::Checkbox).build();
        let json_field = FieldDefinition::builder("meta", FieldType::Json).build();
        let list = FieldDefinition::builder("scores", FieldType::Number)
            .has_many(true)
            .build();
        let date = FieldDefinition::builder("starts", FieldType::Date)
            .timezone(true)
            .build();

        assert_eq!(stored_value(&checkbox, &json!(true), None), json!(true));
        assert_eq!(stored_value(&checkbox, &json!("off"), None), json!(false));
        assert_eq!(
            stored_value(&json_field, &json!({ "n": 1 }), None),
            json!({ "n": 1 })
        );
        assert_eq!(stored_value(&list, &json!(["1", 2.0]), None), json!([1, 2]));
        assert_eq!(
            stored_value(&date, &json!("2026-01-01T09:00"), Some("Europe/Berlin")),
            json!("2026-01-01T08:00:00.000Z")
        );
    }

    /// Regression: a checkbox column read as `1`/`0` while a checkbox inside a
    /// JSON-stored row read as `true`/`false`. The column decodes to a boolean
    /// — an unset column to `false` — and decoding a decoded value changes
    /// nothing.
    #[test]
    fn a_checkbox_column_decodes_to_a_boolean() {
        let checkbox = FieldDefinition::builder("done", FieldType::Checkbox).build();

        assert!(decodes(&checkbox));
        assert_eq!(decode_value(&checkbox, &json!(1)), json!(true));
        assert_eq!(decode_value(&checkbox, &json!(0)), json!(false));
        assert_eq!(decode_value(&checkbox, &Value::Null), json!(false));
        assert_eq!(decode_value(&checkbox, &json!(true)), json!(true));
        assert_eq!(decode_value(&checkbox, &json!(false)), json!(false));
    }

    /// Regression: a JSON column read as its text while the same value inside
    /// an array row read parsed. JSON text — a JSON field's, a JSON-format rich
    /// text's, a has-many reference's id list inside a row — decodes to the
    /// value it spells; a text that isn't JSON is kept, and a decoded value
    /// decodes to itself.
    #[test]
    fn json_text_decodes_to_the_value_it_spells() {
        let json_field = FieldDefinition::builder("meta", FieldType::Json).build();
        let json_richtext = FieldDefinition::builder("body", FieldType::Richtext)
            .admin(FieldAdmin::builder().richtext_format("json").build())
            .build();
        let html_richtext = FieldDefinition::builder("body", FieldType::Richtext).build();
        let refs = FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build();

        assert_eq!(
            decode_value(&json_field, &json!("{\"n\":1}")),
            json!({ "n": 1 })
        );
        assert_eq!(
            decode_value(&json_field, &json!({ "n": 1 })),
            json!({ "n": 1 })
        );
        assert_eq!(decode_value(&json_field, &json!("[1, 2]")), json!([1, 2]));
        assert_eq!(
            decode_value(&json_field, &json!("not json")),
            json!("not json")
        );
        assert_eq!(decode_value(&json_field, &Value::Null), Value::Null);
        assert_eq!(
            decode_value(&json_richtext, &json!("{\"type\":\"doc\"}")),
            json!({ "type": "doc" })
        );
        assert_eq!(
            decode_value(&html_richtext, &json!("{\"type\":\"doc\"}")),
            json!("{\"type\":\"doc\"}"),
            "HTML rich text is text"
        );
        assert_eq!(
            decode_value(&refs, &json!("[\"t1\",\"t2\"]")),
            json!(["t1", "t2"])
        );
    }

    /// A JSON value inside a JSON-stored row is stored in its parsed form: a
    /// text that spells JSON is parsed once, an object is kept, a text that
    /// isn't JSON stays text — so an admin re-save of an API-written object
    /// stores the same value.
    #[test]
    fn a_nested_json_value_is_stored_parsed() {
        let json_field = FieldDefinition::builder("meta", FieldType::Json).build();

        assert_eq!(
            nested_value(&json_field, &json!("{\"n\": 1}"), None),
            json!({ "n": 1 })
        );
        assert_eq!(
            nested_value(&json_field, &json!({ "n": 1 }), None),
            json!({ "n": 1 })
        );
        assert_eq!(
            nested_value(&json_field, &json!("\"{\\\"n\\\":1}\""), None),
            json!("{\"n\":1}"),
            "a JSON text spelling a string is parsed once, not twice"
        );
        assert_eq!(
            nested_value(&json_field, &json!("not json"), None),
            json!("not json")
        );
        assert_eq!(nested_value(&json_field, &json!(""), None), Value::Null);
    }

    /// An empty has-many list spelled as an empty object — the only shape a Lua
    /// table with no entries can arrive as — stores and nests as an empty list,
    /// for a scalar list and a reference list alike.
    #[test]
    fn an_empty_object_is_an_empty_has_many_list() {
        let list = FieldDefinition::builder("tags", FieldType::Text)
            .has_many(true)
            .build();
        let refs = FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build();

        assert_eq!(
            column_value(&list, &json!({}), None),
            DbValue::Text("[]".into())
        );
        assert_eq!(
            column_value(&refs, &json!({}), None),
            DbValue::Text("[]".into())
        );
        assert_eq!(nested_value(&list, &json!({}), None), json!([]));
        assert_eq!(nested_value(&refs, &json!({}), None), json!([]));
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
