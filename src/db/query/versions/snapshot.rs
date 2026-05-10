//! Snapshot building and data extraction helpers.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::core::{Document, DocumentFields, FieldDefinition, FieldType};
use crate::db::{
    DbConnection,
    query::{
        helpers::{prefixed_name, tz_column},
        join::hydrate_document,
    },
};

/// Build a JSON snapshot of a document's current state (fields + join data).
pub fn build_snapshot(
    conn: &dyn DbConnection,
    slug: &str,
    fields: &[FieldDefinition],
    doc: &Document,
) -> Result<Value> {
    let mut hydrated = doc.clone();
    hydrate_document(conn, slug, fields, &mut hydrated, None, None)?;

    let mut data: Map<String, Value> = hydrated.fields.into_iter().collect();

    if let Some(ts) = &doc.created_at {
        data.insert("created_at".to_string(), Value::String(ts.clone()));
    }
    if let Some(ts) = &doc.updated_at {
        data.insert("updated_at".to_string(), Value::String(ts.clone()));
    }

    Ok(Value::Object(data))
}

/// Whether a snapshot JSON value is a scalar that maps to a column write.
/// Arrays / objects are handled via join tables and skipped here.
fn is_scalar_snapshot_value(val: &Value) -> bool {
    !matches!(val, Value::Array(_) | Value::Object(_))
}

/// Extract flat field data from a snapshot for the UPDATE statement.
/// Group fields are always expanded to `field__subfield` sub-columns.
/// Handles both flat (`seo__meta_title`) and nested (`seo: { meta_title: ... }`) snapshot formats.
pub(super) fn extract_snapshot_data(
    obj: &Map<String, Value>,
    fields: &[FieldDefinition],
    locales_enabled: bool,
) -> DocumentFields {
    extract_snapshot_recursive(obj, fields, locales_enabled, "", false)
}

/// Inner recursive extraction with prefix support.
/// `prefix` uses the standard `prefixed_name` convention (no trailing `__`).
fn extract_snapshot_recursive(
    obj: &Map<String, Value>,
    fields: &[FieldDefinition],
    locales_enabled: bool,
    prefix: &str,
    inherited_localized: bool,
) -> DocumentFields {
    let mut data = DocumentFields::new();

    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = prefixed_name(prefix, &field.name);

                data.extend(extract_snapshot_recursive(
                    obj,
                    &field.fields,
                    locales_enabled,
                    &new_prefix,
                    inherited_localized || field.localized,
                ));

                // Also try nested object format (e.g., `seo: { title: ... }`)
                if let Some(nested_obj) = obj.get(&field.name).and_then(|v| v.as_object()) {
                    for (k, v) in extract_snapshot_recursive(
                        nested_obj,
                        &field.fields,
                        locales_enabled,
                        &new_prefix,
                        inherited_localized || field.localized,
                    ) {
                        data.entry(k).or_insert(v);
                    }
                }
            }

            FieldType::Row | FieldType::Collapsible => {
                data.extend(extract_snapshot_recursive(
                    obj,
                    &field.fields,
                    locales_enabled,
                    prefix,
                    inherited_localized,
                ));
            }

            FieldType::Tabs => {
                for tab in &field.tabs {
                    data.extend(extract_snapshot_recursive(
                        obj,
                        &tab.fields,
                        locales_enabled,
                        prefix,
                        inherited_localized,
                    ));
                }
            }

            _ => {
                if !field.has_parent_column() {
                    continue;
                }

                let is_localized = (inherited_localized || field.localized) && locales_enabled;

                if is_localized {
                    continue;
                }

                let key = prefixed_name(prefix, &field.name);

                if let Some(val) = obj.get(&key).or_else(|| obj.get(&field.name))
                    && is_scalar_snapshot_value(val)
                {
                    data.insert(key.clone(), val.clone());
                }

                if field.field_type == FieldType::Date && field.timezone {
                    let tz_key = tz_column(&key);

                    if let Some(tz_val) = obj
                        .get(&tz_key)
                        .or_else(|| obj.get(&tz_column(&field.name)))
                        && is_scalar_snapshot_value(tz_val)
                    {
                        data.insert(tz_key, tz_val.clone());
                    }
                }
            }
        }
    }

    data
}

/// Recursively collect join table data (Blocks/Arrays/Relationships) from a snapshot,
/// including fields nested inside Tabs/Row/Collapsible layout wrappers.
pub(super) fn collect_join_data_from_snapshot(
    fields: &[FieldDefinition],
    obj: &Map<String, Value>,
    join_data: &mut DocumentFields,
) {
    for field in fields {
        match field.field_type {
            FieldType::Row | FieldType::Collapsible => {
                collect_join_data_from_snapshot(&field.fields, obj, join_data);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_join_data_from_snapshot(&tab.fields, obj, join_data);
                }
            }
            _ => {
                if !field.has_parent_column()
                    && let Some(v) = obj.get(&field.name)
                {
                    join_data.insert(field.name.clone(), v.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::core::{FieldDefinition, FieldTab, RelationshipConfig};
    #[test]
    fn extract_snapshot_data_basic() {
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("count", FieldType::Number).build(),
        ];

        let obj: Map<String, Value> =
            serde_json::from_value(json!({"title": "Hello", "count": 42})).unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);
        assert_eq!(data.get("title"), Some(&json!("Hello")));
        assert_eq!(data.get("count"), Some(&json!(42)));
    }

    #[test]
    fn extract_snapshot_data_skips_localized_when_enabled() {
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("slug", FieldType::Text).build(),
        ];

        let obj: Map<String, Value> =
            serde_json::from_value(json!({"title": "Hello", "slug": "hello"})).unwrap();

        let data = extract_snapshot_data(&obj, &fields, true);
        assert!(
            !data.contains_key("title"),
            "localized field should be skipped"
        );
        assert_eq!(data.get("slug"), Some(&json!("hello")));
    }

    #[test]
    fn extract_snapshot_data_group_fields() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                ])
                .build(),
        ];

        // Flat format: seo__title
        let obj: Map<String, Value> =
            serde_json::from_value(json!({"seo__title": "SEO Title"})).unwrap();
        let data = extract_snapshot_data(&obj, &fields, false);
        assert_eq!(data.get("seo__title"), Some(&json!("SEO Title")));

        // Nested format: seo: { title: "..." }
        let obj2: Map<String, Value> =
            serde_json::from_value(json!({"seo": {"title": "Nested SEO"}})).unwrap();
        let data2 = extract_snapshot_data(&obj2, &fields, false);
        assert_eq!(data2.get("seo__title"), Some(&json!("Nested SEO")));
    }

    #[test]
    fn extract_snapshot_data_tabs_promotes_sub_fields() {
        // Fields inside Tabs should be promoted as top-level columns (no prefix)
        let fields = vec![
            FieldDefinition::builder("page_settings", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Settings",
                    vec![
                        FieldDefinition::builder("template", FieldType::Select).build(),
                        FieldDefinition::builder("show_in_nav", FieldType::Checkbox).build(),
                    ],
                )])
                .build(),
        ];

        let obj: Map<String, Value> =
            serde_json::from_value(json!({"template": "landing", "show_in_nav": true})).unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);
        assert_eq!(data.get("template"), Some(&json!("landing")));
        assert_eq!(data.get("show_in_nav"), Some(&json!(true)));
    }

    #[test]
    fn extract_snapshot_data_row_promotes_sub_fields() {
        let fields = vec![
            FieldDefinition::builder("main_row", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("width", FieldType::Number).build(),
                ])
                .build(),
        ];

        let obj: Map<String, Value> = serde_json::from_value(json!({"width": 100})).unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);
        assert_eq!(data.get("width"), Some(&json!(100)));
    }

    #[test]
    fn extract_snapshot_data_nested_row_in_tabs() {
        // Regression: Row inside Tabs at the collection top level was not recursed
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "General",
                    vec![
                        FieldDefinition::builder("inner_row", FieldType::Row)
                            .fields(vec![
                                FieldDefinition::builder("title", FieldType::Text).build(),
                                FieldDefinition::builder("slug", FieldType::Text).build(),
                            ])
                            .build(),
                    ],
                )])
                .build(),
        ];

        let obj: Map<String, Value> =
            serde_json::from_value(json!({"title": "Hello", "slug": "hello"})).unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);
        assert_eq!(
            data.get("title"),
            Some(&json!("Hello")),
            "Row inside Tabs must be recursed"
        );
        assert_eq!(data.get("slug"), Some(&json!("hello")));
    }

    #[test]
    fn collect_join_data_from_snapshot_tabs() {
        // Blocks inside Tabs should be collected as join data
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("page_settings", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Content",
                    vec![FieldDefinition::builder("content", FieldType::Blocks).build()],
                )])
                .build(),
        ];

        let obj: Map<String, Value> = serde_json::from_value(json!({
            "title": "Hello",
            "content": [{"_block_type": "hero", "heading": "Welcome"}]
        }))
        .unwrap();

        let mut join_data = DocumentFields::new();
        collect_join_data_from_snapshot(&fields, &obj, &mut join_data);

        assert!(
            !join_data.contains_key("title"),
            "scalar field should not be in join data"
        );
        assert!(
            join_data.contains_key("content"),
            "blocks inside Tabs must be in join data"
        );
        let blocks = join_data["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["_block_type"], "hero");
    }

    #[test]
    fn collect_join_data_from_snapshot_row_and_collapsible() {
        let fields = vec![
            FieldDefinition::builder("row_wrapper", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("items", FieldType::Array).build(),
                ])
                .build(),
            FieldDefinition::builder("advanced", FieldType::Collapsible)
                .fields(vec![
                    FieldDefinition::builder("related", FieldType::Relationship)
                        .relationship(RelationshipConfig::new("tags", true))
                        .build(),
                ])
                .build(),
        ];

        let obj: Map<String, Value> = serde_json::from_value(json!({
            "items": [{"label": "A"}],
            "related": ["t1", "t2"]
        }))
        .unwrap();

        let mut join_data = DocumentFields::new();
        collect_join_data_from_snapshot(&fields, &obj, &mut join_data);

        assert!(
            join_data.contains_key("items"),
            "array inside Row must be in join data"
        );
        assert!(
            join_data.contains_key("related"),
            "has-many inside Collapsible must be in join data"
        );
    }

    // ── Timezone companion column tests ──────────────────────────────

    #[test]
    fn extract_snapshot_data_includes_tz_companion() {
        // Regression: extract_snapshot_data must extract _tz companion columns
        // for Date fields with timezone: true, so version restore works.
        let fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .timezone(true)
                .build(),
        ];

        let obj: Map<String, Value> = serde_json::from_value(json!({
            "start_date": "2024-06-15T14:00:00.000Z",
            "start_date_tz": "America/New_York"
        }))
        .unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);

        assert_eq!(
            data.get("start_date"),
            Some(&json!("2024-06-15T14:00:00.000Z")),
            "Date value should be extracted"
        );
        assert_eq!(
            data.get("start_date_tz"),
            Some(&json!("America/New_York")),
            "Timezone companion should be extracted"
        );
    }

    #[test]
    fn extract_snapshot_data_date_without_tz_no_companion() {
        // Date field without timezone: true should NOT extract a _tz column.
        let fields = vec![FieldDefinition::builder("event_date", FieldType::Date).build()];

        let obj: Map<String, Value> = serde_json::from_value(json!({
            "event_date": "2024-06-15T14:00:00.000Z"
        }))
        .unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);

        assert_eq!(
            data.get("event_date"),
            Some(&json!("2024-06-15T14:00:00.000Z"))
        );
        assert!(
            !data.contains_key("event_date_tz"),
            "No _tz column should be extracted for non-timezone date"
        );
    }

    #[test]
    fn extract_snapshot_data_group_date_tz_companion() {
        // Date field with timezone inside a Group: the _tz companion should
        // be extracted with the group prefix.
        let fields = vec![
            FieldDefinition::builder("schedule", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("start", FieldType::Date)
                        .timezone(true)
                        .build(),
                ])
                .build(),
        ];

        let obj: Map<String, Value> = serde_json::from_value(json!({
            "schedule__start": "2024-06-15T07:00:00.000Z",
            "schedule__start_tz": "Europe/Berlin"
        }))
        .unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);

        assert_eq!(
            data.get("schedule__start"),
            Some(&json!("2024-06-15T07:00:00.000Z"))
        );
        assert_eq!(
            data.get("schedule__start_tz"),
            Some(&json!("Europe/Berlin")),
            "Group _tz companion should be extracted with prefix"
        );
    }
}
