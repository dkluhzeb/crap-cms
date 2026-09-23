//! List-table cells: the value each column shows for a document.

use serde_json::{Value, json};

use crate::{
    admin::handlers::shared::{date_picker_values, tag_values_of},
    core::{
        FieldDefinition, FieldType, collection::CollectionDefinition, document::Document,
        field::PickerAppearance, json_truthy,
    },
    db::query::helpers::tz_column,
};

/// The value a date cell shows. A field that stores a zone shows its date in
/// that zone — the same conversion the edit form makes — so the list and the
/// editor never disagree about which day a timezone date falls on. Without a
/// stored zone the cell keeps the stored value and `<crap-time>` renders it in
/// the viewer's zone.
fn date_cell_value(field: &FieldDefinition, doc: &Document, stored: &str, format: &str) -> String {
    if !field.has_tz_companion() {
        return stored.to_string();
    }

    let Some(tz) = doc
        .fields
        .get(&tz_column(&field.name))
        .and_then(Value::as_str)
        .filter(|tz| !tz.is_empty())
    else {
        return stored.to_string();
    };

    let (date_only, datetime_local) = date_picker_values(stored, tz, format);

    date_only
        .or(datetime_local)
        .unwrap_or_else(|| stored.to_string())
}

/// The text a Select/Radio cell shows: each stored value's declared label, or
/// the value itself when the field no longer declares it. A `has_many` field
/// shows its whole list — the same values the form shows as tags, which a plain
/// `as_str` on the stored array could not read at all.
fn choice_cell_value(field: &FieldDefinition, raw: &Value) -> String {
    let label_for = |value: &str| {
        field
            .options
            .iter()
            .find(|opt| opt.value == value)
            .map_or_else(
                || value.to_string(),
                |opt| opt.label.resolve_current().to_string(),
            )
    };

    if field.has_many {
        return tag_values_of(raw)
            .iter()
            .map(|value| label_for(value))
            .collect::<Vec<_>>()
            .join(", ");
    }

    label_for(raw.as_str().unwrap_or(""))
}

/// Pre-compute cell values for a document row, parallel to the columns array.
pub(in crate::admin::handlers::collections) fn compute_cells(
    doc: &Document,
    columns: &[Value],
    def: &CollectionDefinition,
) -> Vec<Value> {
    columns
        .iter()
        .map(|col| {
            let key = col["key"].as_str().unwrap_or("");
            match key {
                "_status" => {
                    let status = doc
                        .fields
                        .get("_status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("published");

                    json!({ "value": status, "is_badge": true })
                }
                "created_at" => {
                    json!({ "value": doc.created_at, "is_date": true })
                }
                "updated_at" => {
                    json!({ "value": doc.updated_at, "is_date": true })
                }
                _ => {
                    let field_def = def.fields.iter().find(|f| f.name == key);
                    let raw = doc.fields.get(key).cloned().unwrap_or(Value::Null);

                    if let Some(f) = field_def {
                        match f.field_type {
                            FieldType::Checkbox => {
                                json!({ "value": json_truthy(&raw), "is_bool": true })
                            }
                            FieldType::Date => {
                                let stored = raw.as_str().unwrap_or("");
                                // The cell tells `<crap-time>` how the value was
                                // stored so a day-only date renders as a calendar
                                // date (in UTC), not a local timestamp.
                                let format = f
                                    .picker_appearance
                                    .as_ref()
                                    .map_or("dayOnly", PickerAppearance::as_str);
                                let val = date_cell_value(f, doc, stored, format);

                                json!({ "value": val, "is_date": true, "format": format })
                            }
                            FieldType::Select | FieldType::Radio => {
                                json!({ "value": choice_cell_value(f, &raw) })
                            }
                            FieldType::Textarea => {
                                let text = raw.as_str().unwrap_or("");
                                let truncated = match text.char_indices().nth(80) {
                                    Some((i, _)) => format!("{}…", &text[..i]),
                                    None => text.to_string(),
                                };

                                json!({ "value": truncated })
                            }
                            FieldType::Relationship | FieldType::Upload => {
                                // List columns render raw (un-populated) field
                                // data, so we don't have target labels here
                                // (resolving them would be an N+1 per cell —
                                // deliberately out of scope). Render a clean
                                // value rather than raw JSON: the id for a
                                // has-one, an "N linked" summary for has-many
                                // (which stored a JSON array).
                                let value = match &raw {
                                    Value::String(s) => s.clone(),
                                    Value::Array(a) => format!("{} linked", a.len()),
                                    Value::Null => String::new(),
                                    other => other
                                        .as_str()
                                        .map_or_else(|| other.to_string(), str::to_string),
                                };

                                json!({ "value": value })
                            }
                            // A multi-value Text/Number column shows its
                            // elements, the same list the form shows as tags —
                            // printing the raw JSON leaked brackets and quotes
                            // into the table.
                            _ if f.has_many => {
                                json!({ "value": tag_values_of(&raw).join(", ") })
                            }
                            _ => {
                                let val = match &raw {
                                    Value::String(s) => s.clone(),
                                    Value::Number(n) => n.to_string(),
                                    Value::Bool(b) => b.to_string(),
                                    Value::Null => String::new(),
                                    other => other.to_string(),
                                };

                                json!({ "value": val })
                            }
                        }
                    } else {
                        let val = match &raw {
                            Value::String(s) => s.clone(),
                            Value::Null => String::new(),
                            other => other.to_string(),
                        };

                        json!({ "value": val })
                    }
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        admin::handlers::collections::list_helpers::test_helpers::test_collection,
        core::{LocalizedString, SelectOption, document::DocumentBuilder},
    };

    #[test]
    fn compute_cells_status_badge() {
        let def = test_collection();
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields.insert("_status".into(), json!("draft"));

        let columns = vec![json!({"key": "_status"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["is_badge"], true);
        assert_eq!(cells[0]["value"], "draft");
    }

    #[test]
    fn compute_cells_select_shows_label() {
        let def = test_collection();
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields.insert("status".into(), json!("published"));

        let columns = vec![json!({"key": "status"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["value"], "Published");
    }

    #[test]
    fn compute_cells_checkbox() {
        let def = test_collection();
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields.insert("active".into(), json!(1));

        let columns = vec![json!({"key": "active"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["is_bool"], true);
        assert_eq!(cells[0]["value"], true);
    }

    /// A read hook can hand the list any checkbox spelling; the cell must
    /// agree with the write and the edit form on what counts as checked.
    #[test]
    fn compute_cells_checkbox_reads_every_checked_spelling() {
        let def = test_collection();
        let columns = vec![json!({"key": "active"})];

        for (value, checked) in [
            (json!("yes"), true),
            (json!(" On "), true),
            (json!(0.5), true),
            (json!("off"), false),
            (json!(0), false),
        ] {
            let mut doc = DocumentBuilder::new("1").build();
            doc.fields.insert("active".into(), value.clone());

            let cells = compute_cells(&doc, &columns, &def);
            assert_eq!(cells[0]["value"], checked, "{value}");
        }
    }

    #[test]
    fn compute_cells_date() {
        let def = test_collection();
        let doc = DocumentBuilder::new("1")
            .created_at(Some("2024-01-15"))
            .build();

        let columns = vec![json!({"key": "created_at"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["is_date"], true);
    }

    /// A Date field's cell carries how it was stored so the client renders a
    /// day-only date as a calendar date, not a local timestamp.
    #[test]
    fn compute_cells_date_field_carries_its_picker_format() {
        let mut def = test_collection();
        def.fields.push(
            FieldDefinition::builder("event_date", FieldType::Date)
                .picker_appearance(PickerAppearance::DayOnly)
                .build(),
        );
        def.fields.push(
            FieldDefinition::builder("starts_at", FieldType::Date)
                .picker_appearance(PickerAppearance::DayAndTime)
                .build(),
        );
        def.fields
            .push(FieldDefinition::builder("bare", FieldType::Date).build());
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields
            .insert("event_date".into(), json!("2026-01-15T12:00:00.000Z"));
        doc.fields
            .insert("starts_at".into(), json!("2026-01-15T09:30:00.000Z"));
        doc.fields
            .insert("bare".into(), json!("2026-01-15T12:00:00.000Z"));

        let columns = vec![
            json!({"key": "event_date"}),
            json!({"key": "starts_at"}),
            json!({"key": "bare"}),
        ];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["format"], "dayOnly");
        assert_eq!(cells[1]["format"], "dayAndTime");
        assert_eq!(
            cells[2]["format"], "dayOnly",
            "the parser default is day-only"
        );
    }

    /// Regression: a `has_many` Select cell read the stored array with
    /// `as_str` and always came out empty. It shows every stored value's
    /// label, and a value the field no longer declares shows as itself.
    #[test]
    fn compute_cells_has_many_choice_shows_every_label() {
        let mut def = test_collection();
        def.fields.push(
            FieldDefinition::builder("skills", FieldType::Select)
                .has_many(true)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Design".into()), "design"),
                    SelectOption::new(LocalizedString::Plain("Motion".into()), "motion"),
                ])
                .build(),
        );
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields
            .insert("skills".into(), json!(["design", "motion", "retired"]));

        let columns = vec![json!({"key": "skills"})];
        let cells = compute_cells(&doc, &columns, &def);

        assert_eq!(cells[0]["value"], "Design, Motion, retired");
    }

    /// Regression: a `has_many` Text/Number cell printed the raw JSON, so the
    /// table showed `["a","b"]` where the form showed two tags.
    #[test]
    fn compute_cells_has_many_scalar_shows_its_elements() {
        let mut def = test_collection();
        def.fields.push(
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .build(),
        );
        def.fields.push(
            FieldDefinition::builder("scores", FieldType::Number)
                .has_many(true)
                .build(),
        );
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields.insert("tags".into(), json!(["rust", "cms"]));
        doc.fields.insert("scores".into(), json!([1, 2]));

        let columns = vec![json!({"key": "tags"}), json!({"key": "scores"})];
        let cells = compute_cells(&doc, &columns, &def);

        assert_eq!(cells[0]["value"], "rust, cms");
        assert_eq!(cells[1]["value"], "1, 2");
    }

    /// Regression: a timezone date's cell ignored the stored `_tz`, so the
    /// list rendered it in the viewer's zone while the form showed it in the
    /// stored one — the same instant on two different days.
    #[test]
    fn compute_cells_date_shows_a_stored_zone() {
        let mut def = test_collection();
        def.fields.push(
            FieldDefinition::builder("starts_at", FieldType::Date)
                .timezone(true)
                .picker_appearance(PickerAppearance::DayOnly)
                .build(),
        );
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields
            .insert("starts_at".into(), json!("2026-01-15T20:00:00.000Z"));
        doc.fields
            .insert("starts_at_tz".into(), json!("Asia/Tokyo"));

        let columns = vec![json!({"key": "starts_at"})];
        let cells = compute_cells(&doc, &columns, &def);

        assert_eq!(
            cells[0]["value"], "2026-01-16",
            "the stored zone puts the instant on the next day"
        );
    }

    /// A date field without a stored zone keeps the stored value — the client
    /// formats it in the viewer's zone, as before.
    #[test]
    fn compute_cells_date_without_a_zone_is_unchanged() {
        let mut def = test_collection();
        def.fields
            .push(FieldDefinition::builder("plain_at", FieldType::Date).build());
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields
            .insert("plain_at".into(), json!("2026-01-15T20:00:00.000Z"));

        let columns = vec![json!({"key": "plain_at"})];
        let cells = compute_cells(&doc, &columns, &def);

        assert_eq!(cells[0]["value"], "2026-01-15T20:00:00.000Z");
    }
}
