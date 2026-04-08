//! Extract join table data from form submissions for has-many relationships and array fields.

use serde_json::Value;
use std::collections::HashMap;

use crate::core::field::{FieldDefinition, FieldType};

use super::composite::parse_composite_form_data;

/// Extract join table data from form submission for has-many relationships and array fields.
/// Returns a map suitable for `query::save_join_table_data`.
pub(crate) fn extract_join_data_from_form(
    form: &HashMap<String, String>,
    field_defs: &[FieldDefinition],
) -> HashMap<String, Value> {
    let mut join_data = HashMap::new();

    extract_join_data_recursive(form, field_defs, "", &mut join_data);

    join_data
}

/// Recursive helper for `extract_join_data_from_form`.
/// `prefix` accumulates `__`-separated Group names for nested Groups.
/// Layout wrappers (Row/Collapsible/Tabs) pass through transparently.
fn extract_join_data_recursive(
    form: &HashMap<String, String>,
    field_defs: &[FieldDefinition],
    prefix: &str,
    join_data: &mut HashMap<String, Value>,
) {
    for field in field_defs {
        let full_name = if prefix.is_empty() {
            field.name.clone()
        } else {
            format!("{}__{}", prefix, field.name)
        };

        match field.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship
                    && rc.has_many
                {
                    if let Some(val) = form.get(&full_name) {
                        join_data.insert(full_name, Value::String(val.clone()));
                    } else {
                        join_data.insert(full_name, Value::String(String::new()));
                    }
                }
            }
            FieldType::Array => {
                let json_rows = parse_composite_form_data(form, &full_name, &field.fields);
                join_data.insert(full_name, Value::Array(json_rows));
            }
            FieldType::Blocks => {
                let json_rows = parse_composite_form_data(form, &full_name, &[]);
                join_data.insert(full_name, Value::Array(json_rows));
            }
            FieldType::Group => {
                extract_join_data_recursive(form, &field.fields, &full_name, join_data);
            }
            FieldType::Row | FieldType::Collapsible => {
                extract_join_data_recursive(form, &field.fields, prefix, join_data);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    extract_join_data_recursive(form, &tab.fields, prefix, join_data);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldDefinition, FieldTab, FieldType};

    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, ft).build()
    }

    #[test]
    fn extract_join_data_nested_array() {
        let mut form = HashMap::new();
        form.insert("slides[0][title]".to_string(), "Slide 1".to_string());
        form.insert("slides[0][images][0][url]".to_string(), "a.jpg".to_string());
        form.insert("slides[0][images][1][url]".to_string(), "b.jpg".to_string());

        let mut images_field = make_field("images", FieldType::Array);
        images_field.fields = vec![make_field("url", FieldType::Text)];
        let mut slides_field = make_field("slides", FieldType::Array);
        slides_field.fields = vec![make_field("title", FieldType::Text), images_field];

        let result = extract_join_data_from_form(&form, &[slides_field]);
        let slides = result.get("slides").unwrap().as_array().unwrap();
        assert_eq!(slides.len(), 1);
        assert_eq!(slides[0]["title"], "Slide 1");

        let images = slides[0]["images"].as_array().unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0]["url"], "a.jpg");
        assert_eq!(images[1]["url"], "b.jpg");
    }

    #[test]
    fn extract_join_data_blocks_inside_tabs() {
        let mut form = HashMap::new();
        form.insert("content[0][_block_type]".to_string(), "hero".to_string());
        form.insert("content[0][heading]".to_string(), "Welcome".to_string());
        form.insert("content[1][_block_type]".to_string(), "text".to_string());
        form.insert("content[1][body]".to_string(), "Hello world".to_string());

        let blocks_field = make_field("content", FieldType::Blocks);
        let mut tabs_field = make_field("page_settings", FieldType::Tabs);
        tabs_field.tabs = vec![FieldTab::new("Content", vec![blocks_field])];

        let result =
            extract_join_data_from_form(&form, &[make_field("title", FieldType::Text), tabs_field]);
        let content = result
            .get("content")
            .expect("blocks inside tabs must be extracted");
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["_block_type"], "hero");
        assert_eq!(arr[0]["heading"], "Welcome");
        assert_eq!(arr[1]["_block_type"], "text");
        assert_eq!(arr[1]["body"], "Hello world");
    }

    #[test]
    fn extract_join_data_blocks_inside_row() {
        let mut form = HashMap::new();
        form.insert("items[0][_block_type]".to_string(), "card".to_string());
        form.insert("items[0][title]".to_string(), "Card 1".to_string());

        let blocks_field = make_field("items", FieldType::Blocks);
        let mut row_field = make_field("layout", FieldType::Row);
        row_field.fields = vec![blocks_field];

        let result = extract_join_data_from_form(&form, &[row_field]);
        let items = result
            .get("items")
            .expect("blocks inside row must be extracted");
        assert_eq!(items.as_array().unwrap().len(), 1);
        assert_eq!(items[0]["_block_type"], "card");
    }

    #[test]
    fn extract_join_data_array_inside_collapsible() {
        let mut form = HashMap::new();
        form.insert(
            "links[0][url]".to_string(),
            "https://example.com".to_string(),
        );
        form.insert("links[0][label]".to_string(), "Example".to_string());

        let mut array_field = make_field("links", FieldType::Array);
        array_field.fields = vec![
            make_field("url", FieldType::Text),
            make_field("label", FieldType::Text),
        ];
        let mut collapsible = make_field("sidebar", FieldType::Collapsible);
        collapsible.fields = vec![array_field];

        let result = extract_join_data_from_form(&form, &[collapsible]);
        let links = result
            .get("links")
            .expect("array inside collapsible must be extracted");
        assert_eq!(links.as_array().unwrap().len(), 1);
        assert_eq!(links[0]["url"], "https://example.com");
    }

    #[test]
    fn extract_join_data_array_inside_group() {
        let mut form = HashMap::new();
        form.insert("config__items[0][name]".to_string(), "foo".to_string());
        form.insert("config__items[1][name]".to_string(), "bar".to_string());

        let mut arr = make_field("items", FieldType::Array);
        arr.fields = vec![make_field("name", FieldType::Text)];
        let group = FieldDefinition::builder("config", FieldType::Group)
            .fields(vec![arr])
            .build();

        let result = extract_join_data_from_form(&form, &[group]);
        let items = result
            .get("config__items")
            .expect("array inside group must be extracted with __ prefix");
        let arr = items.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], "foo");
        assert_eq!(arr[1]["name"], "bar");
    }

    #[test]
    fn extract_join_data_array_inside_group_collapsible() {
        let mut form = HashMap::new();
        form.insert("config__items[0][val]".to_string(), "x".to_string());

        let mut arr = make_field("items", FieldType::Array);
        arr.fields = vec![make_field("val", FieldType::Text)];
        let collapsible = FieldDefinition::builder("wrapper", FieldType::Collapsible)
            .fields(vec![arr])
            .build();
        let group = FieldDefinition::builder("config", FieldType::Group)
            .fields(vec![collapsible])
            .build();

        let result = extract_join_data_from_form(&form, &[group]);
        let items = result
            .get("config__items")
            .expect("array inside group>collapsible must be extracted");
        assert_eq!(items.as_array().unwrap()[0]["val"], "x");
    }

    #[test]
    fn extract_join_data_array_inside_group_tabs() {
        let mut form = HashMap::new();
        form.insert(
            "config__links[0][url]".to_string(),
            "https://a.com".to_string(),
        );

        let mut arr = make_field("links", FieldType::Array);
        arr.fields = vec![make_field("url", FieldType::Text)];
        let tabs = FieldDefinition::builder("sections", FieldType::Tabs)
            .tabs(vec![FieldTab::new("General", vec![arr])])
            .build();
        let group = FieldDefinition::builder("config", FieldType::Group)
            .fields(vec![tabs])
            .build();

        let result = extract_join_data_from_form(&form, &[group]);
        let links = result
            .get("config__links")
            .expect("array inside group>tabs must be extracted");
        assert_eq!(links.as_array().unwrap()[0]["url"], "https://a.com");
    }

    #[test]
    fn extract_join_data_array_inside_nested_groups() {
        let mut form = HashMap::new();
        form.insert(
            "outer__inner__items[0][val]".to_string(),
            "deep".to_string(),
        );

        let mut arr = make_field("items", FieldType::Array);
        arr.fields = vec![make_field("val", FieldType::Text)];
        let inner = FieldDefinition::builder("inner", FieldType::Group)
            .fields(vec![arr])
            .build();
        let outer = FieldDefinition::builder("outer", FieldType::Group)
            .fields(vec![inner])
            .build();

        let result = extract_join_data_from_form(&form, &[outer]);
        let items = result
            .get("outer__inner__items")
            .expect("array inside nested groups must use __ prefix chain");
        assert_eq!(items.as_array().unwrap()[0]["val"], "deep");
    }
}
