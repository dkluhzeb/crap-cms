//! Rewriting a document's values (and its array rows) into the form their
//! writes store.

use std::slice;

use serde_json::Value;

use crate::{
    core::{
        DocumentFields, FieldChildren, FieldDefinition, JsonRoot, field_children,
        flatten_array_sub_fields, prefixed_name, walk_leaf_fields,
    },
    db::{
        DbValue,
        query::{
            helpers::{companion_value, stored_value, tz_column},
            join::{store_nested_values, store_rows, sub_field_stores_json},
        },
    },
};

/// Replace every value of `data` (flat keys) with the value its write stores, as
/// a read returns it — so data kept outside the table, a draft snapshot, holds
/// what the stored row would: columns in their column form, array rows with
/// their own columns, and everything stored as JSON in its typed form.
pub(crate) fn stored_document_values(data: &mut DocumentFields, fields: &[FieldDefinition]) {
    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, _| {
        let name = prefixed_name(prefix, &field.name);

        match field_children(field) {
            FieldChildren::Array(sub) => {
                if let Some(Value::Array(rows)) = data.get_mut(&name) {
                    stored_array_rows(sub, rows);
                }
            }
            FieldChildren::Blocks(_) => {
                if let Some(Value::Array(rows)) = data.get_mut(&name) {
                    store_rows(field, rows);
                }
            }
            _ if field.has_parent_column() => store_column(data, &name, field),
            _ => {}
        }

        Ok(())
    });
}

/// Store the column value at `key` of `level` — and every companion column the
/// field stores beside it — in the form its write stores it.
fn store_column<R: JsonRoot>(level: &mut R, key: &str, field: &FieldDefinition) {
    let zone = companion_value(level.root_get(&tz_column(key)));

    if let Some(value) = level.root_get(key) {
        let stored = stored_value(field, value, zone_str(&zone));
        level.root_insert(key.to_string(), stored);
    }

    for column in field.companion_columns(key) {
        if level.root_get(&column).is_none() {
            continue;
        }

        let stored = companion_value(level.root_get(&column)).to_json();
        level.root_insert(column, stored);
    }
}

/// An array's rows as stored: each sub-field with its own column in its column
/// form, the groups, arrays and blocks stored as JSON in their typed form, and
/// JSON-stored values (a JSON field, a has-many reference list) as sent.
fn stored_array_rows(sub: &[FieldDefinition], rows: &mut [Value]) {
    let subs = flatten_array_sub_fields(sub);

    for row in rows.iter_mut().filter_map(Value::as_object_mut) {
        for sf in &subs {
            match field_children(sf) {
                FieldChildren::Group(_) | FieldChildren::Array(_) | FieldChildren::Blocks(_) => {
                    store_nested_values(row, slice::from_ref(*sf));
                }
                _ if sub_field_stores_json(sf) => {}
                _ => store_column(row, &sf.name, sf),
            }
        }
    }
}

/// The zone text of a `_tz` column value.
fn zone_str(zone: &DbValue) -> Option<&str> {
    match zone {
        DbValue::Text(zone) => Some(zone),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::{FieldAdmin, FieldType};

    fn code_with_languages(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Code)
            .admin(
                FieldAdmin::builder()
                    .languages(vec!["python".to_string()])
                    .build(),
            )
            .build()
    }

    /// Every companion of a stored document is normalized the way its write
    /// stores it, not just a date's zone: a blank language pick is NULL, as the
    /// column would hold it, and a real pick is kept.
    #[test]
    fn a_blank_language_companion_stores_as_null() {
        let fields = vec![code_with_languages("snippet"), code_with_languages("notes")];

        let mut data = DocumentFields::new();
        data.insert("snippet".to_string(), json!("print(1)"));
        data.insert("snippet_lang".to_string(), json!(""));
        data.insert("notes".to_string(), json!("print(2)"));
        data.insert("notes_lang".to_string(), json!("python"));

        stored_document_values(&mut data, &fields);

        assert_eq!(data.get("snippet_lang"), Some(&Value::Null));
        assert_eq!(data.get("notes_lang"), Some(&json!("python")));
    }

    /// A date's zone keeps normalizing, and the date reads back as the instant
    /// its column holds.
    #[test]
    fn a_blank_timezone_companion_stores_as_null() {
        let fields = vec![
            FieldDefinition::builder("starts", FieldType::Date)
                .timezone(true)
                .build(),
        ];

        let mut data = DocumentFields::new();
        data.insert("starts".to_string(), json!("2026-01-01T09:00"));
        data.insert("starts_tz".to_string(), json!(""));

        stored_document_values(&mut data, &fields);

        assert_eq!(data.get("starts_tz"), Some(&Value::Null));
    }
}
