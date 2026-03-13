//! Snapshot building and data extraction helpers.

use anyhow::Result;
use serde_json::{Map, Value};
use std::collections::HashMap;

use crate::core::{
    Document,
    field::{FieldDefinition, FieldType},
};

/// Build a JSON snapshot of a document's current state (fields + join data).
pub fn build_snapshot(
    conn: &rusqlite::Connection,
    slug: &str,
    fields: &[FieldDefinition],
    doc: &Document,
) -> Result<Value> {
    let mut data = Map::new();
    for (k, v) in &doc.fields {
        data.insert(k.clone(), v.clone());
    }
    // Hydrate join table data into the snapshot
    let mut doc_clone = doc.clone();
    super::super::hydrate_document(conn, slug, fields, &mut doc_clone, None, None)?;
    for (k, v) in &doc_clone.fields {
        data.insert(k.clone(), v.clone());
    }
    if let Some(ref ts) = doc.created_at {
        data.insert("created_at".to_string(), Value::String(ts.clone()));
    }
    if let Some(ref ts) = doc.updated_at {
        data.insert("updated_at".to_string(), Value::String(ts.clone()));
    }
    Ok(Value::Object(data))
}

/// Convert a JSON value to a string for the data HashMap.
/// Returns None for complex types (arrays/objects) that are handled via join tables.
pub(super) fn snapshot_val_to_string(val: Option<&Value>) -> Option<String> {
    match val {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(b.to_string()),
        Some(Value::Null) | None => Some(String::new()),
        _ => None, // complex types (arrays/objects) handled via join tables
    }
}

/// Extract flat field data from a snapshot for the UPDATE statement.
/// Group fields are always expanded to `field__subfield` sub-columns.
/// Handles both flat (`seo__meta_title`) and nested (`seo: { meta_title: ... }`) snapshot formats.
pub(super) fn extract_snapshot_data(
    obj: &Map<String, Value>,
    fields: &[FieldDefinition],
    locales_enabled: bool,
) -> HashMap<String, String> {
    let mut data: HashMap<String, String> = HashMap::new();
    for field in fields {
        if field.field_type == FieldType::Group {
            let nested_obj = obj.get(&field.name).and_then(|v| v.as_object());
            for sub in &field.fields {
                let is_localized = (field.localized || sub.localized) && locales_enabled;

                if is_localized {
                    continue;
                }
                let key = format!("{}__{}", field.name, sub.name);
                // Try flat key first, then nested path
                let val = obj
                    .get(&key)
                    .or_else(|| nested_obj.and_then(|n| n.get(&sub.name)));

                if let Some(s) = snapshot_val_to_string(val) {
                    data.insert(key, s);
                }
            }
            continue;
        }
        // Row/Collapsible fields promote sub-fields as top-level columns (no prefix).
        // Recurse to handle nested layout wrappers (e.g., Row inside Tabs).
        if field.field_type == FieldType::Row || field.field_type == FieldType::Collapsible {
            data.extend(extract_snapshot_data(obj, &field.fields, locales_enabled));
            continue;
        }
        // Tabs fields promote sub-fields from all tabs as top-level columns (no prefix).
        // Recurse to handle nested layout wrappers.
        if field.field_type == FieldType::Tabs {
            for tab in &field.tabs {
                data.extend(extract_snapshot_data(obj, &tab.fields, locales_enabled));
            }
            continue;
        }
        if !field.has_parent_column() {
            continue;
        }
        if field.localized && locales_enabled {
            continue;
        }
        if let Some(s) = snapshot_val_to_string(obj.get(&field.name)) {
            data.insert(field.name.clone(), s);
        }
    }
    data
}

/// Recursively collect join table data (Blocks/Arrays/Relationships) from a snapshot,
/// including fields nested inside Tabs/Row/Collapsible layout wrappers.
pub(super) fn collect_join_data_from_snapshot(
    fields: &[FieldDefinition],
    obj: &Map<String, Value>,
    join_data: &mut HashMap<String, Value>,
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
    use crate::core::field::{FieldDefinition, FieldTab, RelationshipConfig};

    #[test]
    fn snapshot_val_to_string_variants() {
        assert_eq!(
            snapshot_val_to_string(Some(&json!("hello"))),
            Some("hello".to_string())
        );
        assert_eq!(
            snapshot_val_to_string(Some(&json!(42))),
            Some("42".to_string())
        );
        assert_eq!(
            snapshot_val_to_string(Some(&json!(true))),
            Some("true".to_string())
        );
        assert_eq!(
            snapshot_val_to_string(Some(&json!(false))),
            Some("false".to_string())
        );
        assert_eq!(
            snapshot_val_to_string(Some(&Value::Null)),
            Some(String::new())
        );
        assert_eq!(snapshot_val_to_string(None), Some(String::new()));
        // Complex types return None
        assert_eq!(snapshot_val_to_string(Some(&json!([1, 2]))), None);
        assert_eq!(snapshot_val_to_string(Some(&json!({"a": 1}))), None);
    }

    #[test]
    fn extract_snapshot_data_basic() {
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("count", FieldType::Number).build(),
        ];

        let obj: Map<String, Value> =
            serde_json::from_value(json!({"title": "Hello", "count": 42})).unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);
        assert_eq!(data.get("title"), Some(&"Hello".to_string()));
        assert_eq!(data.get("count"), Some(&"42".to_string()));
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
        assert_eq!(data.get("slug"), Some(&"hello".to_string()));
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
        assert_eq!(data.get("seo__title"), Some(&"SEO Title".to_string()));

        // Nested format: seo: { title: "..." }
        let obj2: Map<String, Value> =
            serde_json::from_value(json!({"seo": {"title": "Nested SEO"}})).unwrap();
        let data2 = extract_snapshot_data(&obj2, &fields, false);
        assert_eq!(data2.get("seo__title"), Some(&"Nested SEO".to_string()));
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
        assert_eq!(data.get("template"), Some(&"landing".to_string()));
        assert_eq!(data.get("show_in_nav"), Some(&"true".to_string()));
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
        assert_eq!(data.get("width"), Some(&"100".to_string()));
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
            Some(&"Hello".to_string()),
            "Row inside Tabs must be recursed"
        );
        assert_eq!(data.get("slug"), Some(&"hello".to_string()));
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

        let mut join_data = HashMap::new();
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

        let mut join_data = HashMap::new();
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
}
