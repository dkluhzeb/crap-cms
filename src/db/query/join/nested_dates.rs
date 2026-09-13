//! Convert timezone-enabled dates nested in JSON-stored rows to UTC.
//!
//! Top-level dates and the direct sub-fields of an array row have their own
//! columns and are converted as those columns are written. Everything stored as
//! JSON — a blocks row, a group inside a row, a row of a nested array — carries
//! the same `value` + `value_tz` pair inside the JSON. These walk such a value
//! with its field tree and apply the same conversion, so every timezone date is
//! stored as UTC. A value that already carries an offset (`…Z`) is left as it
//! is, which makes the conversion safe to repeat on every write.

use serde_json::{Map, Value};

use crate::{
    core::{
        BLOCK_TYPE_KEY, BlockDefinition, FieldChildren, FieldDefinition, FieldType, field_children,
    },
    db::query::helpers::{normalize_date_with_timezone, tz_column},
};

/// Convert every timezone date in `obj`, an object keyed by `fields`' names.
pub(crate) fn convert_timezone_dates(fields: &[FieldDefinition], obj: &mut Map<String, Value>) {
    for field in fields {
        match field_children(field) {
            FieldChildren::Leaf => convert_leaf(field, obj),
            FieldChildren::Group(sub) => {
                if let Some(Value::Object(group)) = obj.get_mut(&field.name) {
                    convert_timezone_dates(sub, group);
                }
            }
            FieldChildren::Wrapper(sub) => convert_timezone_dates(sub, obj),
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    convert_timezone_dates(&tab.fields, obj);
                }
            }
            FieldChildren::Array(sub) => {
                if let Some(Value::Array(rows)) = obj.get_mut(&field.name) {
                    for row in rows.iter_mut().filter_map(Value::as_object_mut) {
                        convert_timezone_dates(sub, row);
                    }
                }
            }
            FieldChildren::Blocks(defs) => {
                if let Some(Value::Array(rows)) = obj.get_mut(&field.name) {
                    convert_block_rows(defs, rows);
                }
            }
        }
    }
}

/// Convert every timezone date in a list of block rows, matching each row to
/// its definition by `_block_type`. Rows of an unknown type are left alone.
pub(crate) fn convert_block_rows(defs: &[BlockDefinition], rows: &mut [Value]) {
    for obj in rows.iter_mut().filter_map(Value::as_object_mut) {
        let Some(def) = obj
            .get(BLOCK_TYPE_KEY)
            .and_then(Value::as_str)
            .and_then(|block_type| defs.iter().find(|d| d.block_type == block_type))
        else {
            continue;
        };

        convert_timezone_dates(&def.fields, obj);
    }
}

/// Convert one timezone date leaf in place, using its `{name}_tz` sibling.
fn convert_leaf(field: &FieldDefinition, obj: &mut Map<String, Value>) {
    if field.field_type != FieldType::Date || !field.timezone {
        return;
    }

    let Some(tz) = obj
        .get(&tz_column(&field.name))
        .and_then(Value::as_str)
        .filter(|tz| !tz.is_empty())
        .map(str::to_string)
    else {
        return;
    };

    let Some(Value::String(value)) = obj.get_mut(&field.name) else {
        return;
    };
    if value.is_empty() {
        return;
    }

    // An invalid zone or a local time that does not exist in it is left as
    // written: validation rejects those on write.
    if let Ok(utc) = normalize_date_with_timezone(value, &tz) {
        *value = utc;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

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

        convert_timezone_dates(&fields, &mut obj);

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

        convert_timezone_dates(&fields, &mut obj);

        assert_eq!(obj["slots"][0]["starts"], UTC);
        // Summer time: UTC+2.
        assert_eq!(obj["slots"][1]["starts"], "2024-07-15T07:00:00.000Z");
    }

    /// Converting twice is the same as converting once, and dates without a
    /// zone or without `timezone = true` are untouched.
    #[test]
    fn is_idempotent_and_skips_dates_without_a_zone() {
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

        convert_timezone_dates(&fields, &mut obj);
        let once = obj.clone();
        convert_timezone_dates(&fields, &mut obj);

        assert_eq!(obj, once);
        assert_eq!(obj["starts"], UTC);
        assert_eq!(obj["plain"], LOCAL);
        assert_eq!(obj["no_zone"], LOCAL);
    }

    /// Each block row is converted with its own definition; a key the block
    /// does not define is not a date and stays as it is.
    #[test]
    fn converts_dates_in_block_rows_by_their_type() {
        let defs = vec![
            BlockDefinition::new("event", vec![starts()]),
            BlockDefinition::new("note", vec![]),
        ];
        let mut rows = vec![
            json!({ "_block_type": "event", "starts": LOCAL, "starts_tz": "Europe/Berlin" }),
            json!({ "_block_type": "note", "starts": LOCAL, "starts_tz": "Europe/Berlin" }),
        ];

        convert_block_rows(&defs, &mut rows);

        assert_eq!(rows[0]["starts"], UTC);
        assert_eq!(rows[1]["starts"], LOCAL);
    }
}
