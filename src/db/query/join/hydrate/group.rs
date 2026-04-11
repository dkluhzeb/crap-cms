//! Group field reconstruction from prefixed flat document columns.

use serde_json::{Map, Value};

use crate::{
    core::{Document, FieldDefinition, FieldType},
    db::query::helpers::prefixed_name,
};

/// Recursively extract prefixed columns from `doc.fields` into a nested Group object.
/// Handles Group→Row, Group→Collapsible, Group→Tabs, and Group→Group nesting.
pub(super) fn reconstruct_group_fields(
    fields: &[FieldDefinition],
    prefix: &str,
    doc: &mut Document,
    group_obj: &mut Map<String, Value>,
) {
    for sub in fields {
        match sub.field_type {
            FieldType::Group => {
                // Nested group: collect sub-group's fields into a nested object
                let new_prefix = prefixed_name(prefix, &sub.name);
                let mut sub_obj = Map::new();
                reconstruct_group_fields(&sub.fields, &new_prefix, doc, &mut sub_obj);

                if !sub_obj.is_empty() {
                    group_obj.insert(sub.name.clone(), Value::Object(sub_obj));
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                // Layout fields are transparent — promote sub-fields to same level
                reconstruct_group_fields(&sub.fields, prefix, doc, group_obj);
            }
            FieldType::Tabs => {
                for tab in &sub.tabs {
                    reconstruct_group_fields(&tab.fields, prefix, doc, group_obj);
                }
            }
            _ => {
                let col_name = prefixed_name(prefix, &sub.name);

                if let Some(val) = doc.fields.remove(&col_name) {
                    group_obj.insert(sub.name.clone(), val);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldDefinition, FieldTab, FieldType};
    use rusqlite::Connection;
    use serde_json::json;

    use super::super::hydrate_document;

    #[test]
    fn hydrate_group_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                seo__meta_title TEXT,
                seo__meta_desc TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts VALUES ('p1', 'Test', 'SEO Title', 'SEO Desc', '2024-01-01', '2024-01-01');",
        ).unwrap();

        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("meta_title", FieldType::Text).build(),
                    FieldDefinition::builder("meta_desc", FieldType::Text).build(),
                ])
                .build(),
        ];

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), json!("Test"));
        doc.fields
            .insert("seo__meta_title".to_string(), json!("SEO Title"));
        doc.fields
            .insert("seo__meta_desc".to_string(), json!("SEO Desc"));

        hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

        let seo = doc.fields.get("seo").expect("seo group should exist");
        assert_eq!(
            seo.get("meta_title").and_then(|v| v.as_str()),
            Some("SEO Title")
        );
        assert_eq!(
            seo.get("meta_desc").and_then(|v| v.as_str()),
            Some("SEO Desc")
        );
        assert!(!doc.fields.contains_key("seo__meta_title"));
        assert!(!doc.fields.contains_key("seo__meta_desc"));
    }

    #[test]
    fn hydrate_nested_group_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                seo__social__og_title TEXT
            );
            INSERT INTO posts VALUES ('p1', 'OG Title Value');",
        )
        .unwrap();

        let inner_group = FieldDefinition::builder("social", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("og_title", FieldType::Text).build(),
            ])
            .build();
        let outer_group = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![inner_group])
            .build();

        let mut doc = Document::new("p1".to_string());
        doc.fields
            .insert("seo__social__og_title".to_string(), json!("OG Title Value"));

        hydrate_document(&conn, "posts", &[outer_group], &mut doc, None, None).unwrap();

        let seo = doc.fields.get("seo").expect("seo group should exist");
        let social = seo.get("social").expect("nested social group should exist");
        assert_eq!(
            social.get("og_title").and_then(|v| v.as_str()),
            Some("OG Title Value")
        );
    }

    #[test]
    fn hydrate_group_with_row_sub_fields() {
        // A Row inside a Group is transparent — its sub-fields are promoted to the group level
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             INSERT INTO posts (id) VALUES ('p1');",
        )
        .unwrap();

        let row_wrapper = FieldDefinition::builder("layout_row", FieldType::Row)
            .fields(vec![
                FieldDefinition::builder("col_a", FieldType::Text).build(),
                FieldDefinition::builder("col_b", FieldType::Text).build(),
            ])
            .build();
        let outer_group = FieldDefinition::builder("layout", FieldType::Group)
            .fields(vec![row_wrapper])
            .build();

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("layout__col_a".to_string(), json!("A"));
        doc.fields.insert("layout__col_b".to_string(), json!("B"));

        hydrate_document(&conn, "posts", &[outer_group], &mut doc, None, None).unwrap();

        let layout = doc.fields.get("layout").expect("layout group should exist");
        assert_eq!(layout.get("col_a").and_then(|v| v.as_str()), Some("A"));
        assert_eq!(layout.get("col_b").and_then(|v| v.as_str()), Some("B"));
        assert!(
            layout.get("layout_row").is_none(),
            "Row wrapper should be transparent"
        );
    }

    #[test]
    fn hydrate_group_with_tabs_sub_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             INSERT INTO posts (id) VALUES ('p1');",
        )
        .unwrap();

        let tabs_wrapper = FieldDefinition::builder("tabs", FieldType::Tabs)
            .tabs(vec![
                FieldTab::new(
                    "Tab A",
                    vec![FieldDefinition::builder("field_a", FieldType::Text).build()],
                ),
                FieldTab::new(
                    "Tab B",
                    vec![FieldDefinition::builder("field_b", FieldType::Text).build()],
                ),
            ])
            .build();
        let outer_group = FieldDefinition::builder("settings", FieldType::Group)
            .fields(vec![tabs_wrapper])
            .build();

        let mut doc = Document::new("p1".to_string());
        doc.fields
            .insert("settings__field_a".to_string(), json!("val_a"));
        doc.fields
            .insert("settings__field_b".to_string(), json!("val_b"));

        hydrate_document(&conn, "posts", &[outer_group], &mut doc, None, None).unwrap();

        let settings = doc
            .fields
            .get("settings")
            .expect("settings group should exist");
        assert_eq!(
            settings.get("field_a").and_then(|v| v.as_str()),
            Some("val_a")
        );
        assert_eq!(
            settings.get("field_b").and_then(|v| v.as_str()),
            Some("val_b")
        );
    }
}
