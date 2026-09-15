//! The stored form of values inside JSON-stored rows.
//!
//! Top-level fields and the direct sub-fields of an array row have their own
//! columns and are encoded as those columns are written. Everything stored as
//! JSON — a blocks row, a group inside a row, a row of a nested array — holds its
//! values inside the JSON. These walk such a value with its field tree on the
//! shared walker and store every value in one typed form whoever wrote it — the
//! rule is [`nested_value`]: a checkbox as `true`/`false`, a number as a number,
//! a timezone date as UTC. Storing a stored value again changes nothing, so it
//! runs on every write.

use std::{mem, slice};

use serde_json::{Map, Value};

use crate::{
    core::{
        FieldChildren, FieldDefinition, JsonRoot, VisitAction, field_children, walk_nested_mut,
    },
    db::query::helpers::{nested_value, tz_column},
};

/// Store every value of `obj` — an object keyed by `fields`' names, descending
/// into groups, arrays and blocks at any depth — in its typed form. A value that
/// is missing stays missing.
pub(crate) fn store_nested_values<R: JsonRoot>(obj: &mut R, fields: &[FieldDefinition]) {
    walk_nested_mut(obj, fields, &mut Vec::new(), &mut |field, level, _| {
        if !matches!(field_children(field), FieldChildren::Leaf) {
            return VisitAction::Keep;
        }

        let Some(value) = level.root_get(&field.name) else {
            return VisitAction::Keep;
        };

        let zone = level
            .root_get(&tz_column(&field.name))
            .and_then(Value::as_str);
        let stored = nested_value(field, value, zone);

        if stored == *value {
            VisitAction::Keep
        } else {
            VisitAction::Replace(stored)
        }
    });
}

/// Store the rows of an array or blocks `field` in their typed form, each block
/// row with its own block's fields.
pub(crate) fn store_rows(field: &FieldDefinition, rows: &mut Vec<Value>) {
    let mut holder = Map::new();
    holder.insert(field.name.clone(), Value::Array(mem::take(rows)));

    store_nested_values(&mut holder, slice::from_ref(field));

    if let Some(Value::Array(stored)) = holder.remove(&field.name) {
        *rows = stored;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        core::{BlockDefinition, FieldType},
        db::{DbValue, query::helpers::coerce_value},
    };

    fn starts() -> FieldDefinition {
        FieldDefinition::builder("starts", FieldType::Date)
            .timezone(true)
            .build()
    }

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!("fixture is an object"),
        }
    }

    /// Berlin is UTC+1 in January.
    const LOCAL: &str = "2024-01-15T09:00";
    const UTC: &str = "2024-01-15T08:00:00.000Z";

    #[test]
    fn converts_a_date_inside_a_group_and_a_layout_wrapper() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![starts()])
                .build(),
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("ends", FieldType::Date)
                        .timezone(true)
                        .build(),
                ])
                .build(),
        ];
        let mut obj = object(json!({
            "meta": { "starts": LOCAL, "starts_tz": "Europe/Berlin" },
            "ends": LOCAL, "ends_tz": "Europe/Berlin",
        }));

        store_nested_values(&mut obj, &fields);

        assert_eq!(obj["meta"]["starts"], UTC);
        assert_eq!(obj["ends"], UTC);
    }

    #[test]
    fn converts_dates_in_every_row_of_a_nested_array() {
        let fields = vec![
            FieldDefinition::builder("slots", FieldType::Array)
                .fields(vec![starts()])
                .build(),
        ];
        let mut obj = object(json!({
            "slots": [
                { "starts": LOCAL, "starts_tz": "Europe/Berlin" },
                { "starts": "2024-07-15T09:00", "starts_tz": "Europe/Berlin" },
            ],
        }));

        store_nested_values(&mut obj, &fields);

        assert_eq!(obj["slots"][0]["starts"], UTC);
        // Summer time: UTC+2.
        assert_eq!(obj["slots"][1]["starts"], "2024-07-15T07:00:00.000Z");
    }

    /// Storing twice is the same as storing once; a date without a zone, or
    /// without `timezone = true`, is normalized as written — as its column would
    /// hold it.
    #[test]
    fn is_idempotent_and_normalizes_dates_without_a_zone() {
        let fields = vec![
            starts(),
            FieldDefinition::builder("plain", FieldType::Date).build(),
            FieldDefinition::builder("no_zone", FieldType::Date)
                .timezone(true)
                .build(),
        ];
        let mut obj = object(json!({
            "starts": LOCAL, "starts_tz": "Europe/Berlin",
            "plain": LOCAL, "plain_tz": "Europe/Berlin",
            "no_zone": LOCAL,
        }));

        store_nested_values(&mut obj, &fields);
        let once = obj.clone();
        store_nested_values(&mut obj, &fields);

        assert_eq!(obj, once);
        assert_eq!(obj["starts"], UTC);
        let DbValue::Text(plain) = coerce_value(&FieldType::Date, LOCAL) else {
            panic!("a date encodes as text");
        };
        assert_eq!(obj["plain"], json!(plain));
        assert_eq!(obj["no_zone"], json!(plain));
    }

    /// Each block row is stored with its own definition; a key the block does
    /// not define stays as it is.
    #[test]
    fn stores_block_rows_by_their_type() {
        let field = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![
                BlockDefinition::new("event", vec![starts()]),
                BlockDefinition::new("note", vec![]),
            ])
            .build();
        let mut rows = vec![
            json!({ "_block_type": "event", "starts": LOCAL, "starts_tz": "Europe/Berlin" }),
            json!({ "_block_type": "note", "starts": LOCAL, "starts_tz": "Europe/Berlin" }),
        ];

        store_rows(&field, &mut rows);

        assert_eq!(rows[0]["starts"], UTC);
        assert_eq!(rows[1]["starts"], LOCAL);
    }

    /// Regression: values inside JSON-stored rows were kept as sent, so the
    /// admin form's `"on"` and `"3"` were stored — and read — as strings, while
    /// typed writers stored `true` and `3`.
    #[test]
    fn stores_nested_values_in_their_typed_form() {
        let fields = vec![
            FieldDefinition::builder("done", FieldType::Checkbox).build(),
            FieldDefinition::builder("n", FieldType::Number).build(),
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .build(),
            FieldDefinition::builder("note", FieldType::Text).build(),
            FieldDefinition::builder("meta", FieldType::Json).build(),
        ];
        let mut obj = object(json!({
            "done": "on",
            "n": "3",
            "tags": "[\"a\",\"b\"]",
            "note": "",
            "meta": { "k": 1 },
        }));

        store_nested_values(&mut obj, &fields);

        assert_eq!(obj["done"], json!(true));
        assert_eq!(obj["n"], json!(3));
        assert_eq!(obj["tags"], json!(["a", "b"]));
        assert_eq!(obj["note"], Value::Null);
        assert_eq!(obj["meta"], json!({ "k": 1 }));
    }
}
