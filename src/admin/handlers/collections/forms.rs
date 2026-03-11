//! Form parsing helpers: multipart, array fields, upload metadata.

use axum::extract::{FromRequest, Multipart};
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::core::field::{FieldDefinition, FieldType, flatten_array_sub_fields};
use crate::core::upload::{UploadedFile, UploadedFileBuilder};

/// Extract join table data from form submission for has-many relationships and array fields.
/// Returns a map suitable for `query::save_join_table_data`.
pub(crate) fn extract_join_data_from_form(
    form: &HashMap<String, String>,
    field_defs: &[FieldDefinition],
) -> HashMap<String, serde_json::Value> {
    let mut join_data = HashMap::new();

    for field in field_defs {
        match field.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        // Has-many: comma-separated IDs in form value
                        if let Some(val) = form.get(&field.name) {
                            join_data
                                .insert(field.name.clone(), serde_json::Value::String(val.clone()));
                        } else {
                            // Empty selection — clear all
                            join_data.insert(
                                field.name.clone(),
                                serde_json::Value::String(String::new()),
                            );
                        }
                    }
                }
            }
            FieldType::Array => {
                let json_rows = parse_composite_form_data(form, &field.name, &field.fields);
                join_data.insert(field.name.clone(), serde_json::Value::Array(json_rows));
            }
            FieldType::Blocks => {
                let json_rows = parse_composite_form_data(form, &field.name, &[]);
                join_data.insert(field.name.clone(), serde_json::Value::Array(json_rows));
            }
            FieldType::Row | FieldType::Collapsible => {
                let nested = extract_join_data_from_form(form, &field.fields);
                join_data.extend(nested);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    let nested = extract_join_data_from_form(form, &tab.fields);
                    join_data.extend(nested);
                }
            }
            _ => {}
        }
    }

    join_data
}

/// Convert comma-separated form values to JSON arrays for `has_many` select fields.
/// The JS multi-select interceptor joins selected values with commas; this converts
/// them to JSON array strings (e.g., `"a,b"` → `'["a","b"]'`) for storage in TEXT columns.
pub(crate) fn transform_select_has_many(
    form: &mut HashMap<String, String>,
    field_defs: &[FieldDefinition],
) {
    for field in field_defs {
        match field.field_type {
            FieldType::Select | FieldType::Text | FieldType::Number if field.has_many => {
                if let Some(val) = form.get_mut(&field.name) {
                    if val.is_empty() {
                        *val = "[]".to_string();
                    } else {
                        let values: Vec<&str> = val
                            .split(',')
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty())
                            .collect();
                        *val = serde_json::json!(values).to_string();
                    }
                } else {
                    form.insert(field.name.clone(), "[]".to_string());
                }
            }
            // Recurse into group sub-fields (stored as flat group__subfield columns)
            FieldType::Group => {
                let mut has_many_names: Vec<(String, String)> = Vec::new();
                for sf in &field.fields {
                    if matches!(
                        sf.field_type,
                        FieldType::Select | FieldType::Text | FieldType::Number
                    ) && sf.has_many
                    {
                        let full_name = format!("{}__{}", field.name, sf.name);
                        if let Some(val) = form.get(&full_name) {
                            let json_val = if val.is_empty() {
                                "[]".to_string()
                            } else {
                                let values: Vec<&str> = val
                                    .split(',')
                                    .map(|s| s.trim())
                                    .filter(|s| !s.is_empty())
                                    .collect();
                                serde_json::json!(values).to_string()
                            };
                            has_many_names.push((full_name, json_val));
                        } else {
                            has_many_names.push((full_name, "[]".to_string()));
                        }
                    }
                }
                for (name, val) in has_many_names {
                    form.insert(name, val);
                }
            }
            // Row/Collapsible promote sub-fields to the same level — recurse
            FieldType::Row | FieldType::Collapsible => {
                transform_select_has_many(form, &field.fields);
            }
            // Tabs promote sub-fields to the same level — recurse into each tab
            FieldType::Tabs => {
                for tab in &field.tabs {
                    transform_select_has_many(form, &tab.fields);
                }
            }
            _ => {}
        }
    }
}

/// Recursively parse composite form data from flat form keys.
///
/// Handles arbitrarily nested keys like `content[0][items][1][title]`.
/// Uses field definitions to know which sub-fields are composites (need recursion)
/// vs. scalars (leaf values stored as strings).
///
/// Returns a Vec of JSON objects, one per row.
fn parse_composite_form_data(
    form: &HashMap<String, String>,
    field_name: &str,
    sub_field_defs: &[FieldDefinition],
) -> Vec<serde_json::Value> {
    let prefix = format!("{}[", field_name);
    let mut rows: std::collections::BTreeMap<usize, Vec<(String, String)>> =
        std::collections::BTreeMap::new();

    // Collect all form keys that start with this field's prefix
    for (key, value) in form {
        if let Some(rest) = key.strip_prefix(&prefix) {
            // rest looks like "0][title]" or "0][items][1][caption]"
            if let Some(bracket_pos) = rest.find(']') {
                if let Ok(idx) = rest[..bracket_pos].parse::<usize>() {
                    let after = &rest[bracket_pos + 1..];
                    if let Some(remaining) = after.strip_prefix('[') {
                        // remaining is "title]" or "items][1][caption]"
                        // Find the first sub-field name
                        if let Some(next_bracket) = remaining.find(']') {
                            let sub_key = &remaining[..next_bracket];
                            let tail = &remaining[next_bracket + 1..];

                            if tail.is_empty() {
                                // Leaf: simple key like "title]" → key="title", value=form value
                                rows.entry(idx)
                                    .or_default()
                                    .push((sub_key.to_string(), value.clone()));
                            } else {
                                // Nested: "items][1][caption]" → reconstruct the deeper key
                                // Store as sub_key + tail so we can re-parse recursively
                                let deep_key = format!("{}{}", sub_key, tail);
                                rows.entry(idx).or_default().push((deep_key, value.clone()));
                            }
                        }
                    }
                }
            }
        }
    }

    rows.into_values()
        .map(|entries| {
            let mut obj = serde_json::Map::new();

            // Separate leaf entries from nested entries
            let mut nested_keys: HashMap<String, Vec<(String, String)>> = HashMap::new();

            for (key, value) in entries {
                if let Some(bracket_pos) = key.find('[') {
                    // This is a nested key like "items[1][caption]"
                    let base_key = &key[..bracket_pos];
                    let rest = &key[bracket_pos..]; // "[1][caption]"
                    nested_keys
                        .entry(base_key.to_string())
                        .or_default()
                        .push((rest.to_string(), value));
                } else {
                    // Simple leaf key
                    obj.insert(key, serde_json::Value::String(value));
                }
            }

            // Process nested keys recursively
            let flat_defs = flatten_array_sub_fields(sub_field_defs);
            for (base_key, nested_entries) in nested_keys {
                // Look up the field definition for this sub-field to determine type
                let sf_def = flat_defs.iter().find(|sf| sf.name == base_key).copied();
                let nested_sub_defs = sf_def.map(|sf| sf.fields.as_slice()).unwrap_or(&[]);

                // Reconstruct a form-like HashMap with the base_key as prefix
                let mut sub_form = HashMap::new();
                for (rest, value) in &nested_entries {
                    // rest is like "[1][caption]" — reconstruct full key as "base_key[1][caption]"
                    sub_form.insert(format!("{}{}", base_key, rest), value.clone());
                }

                let is_composite = sf_def
                    .map(|sf| {
                        matches!(
                            sf.field_type,
                            FieldType::Array
                                | FieldType::Blocks
                                | FieldType::Group
                                | FieldType::Row
                                | FieldType::Collapsible
                                | FieldType::Tabs
                        )
                    })
                    .unwrap_or(false);

                if is_composite {
                    let nested_rows =
                        parse_composite_form_data(&sub_form, &base_key, nested_sub_defs);
                    // For group/row fields, the "rows" are actually a single object
                    if sf_def
                        .map(|sf| {
                            sf.field_type == FieldType::Group
                                || sf.field_type == FieldType::Row
                                || sf.field_type == FieldType::Collapsible
                                || sf.field_type == FieldType::Tabs
                        })
                        .unwrap_or(false)
                    {
                        if let Some(first) = nested_rows.into_iter().next() {
                            obj.insert(base_key, first);
                        }
                    } else {
                        obj.insert(base_key, serde_json::Value::Array(nested_rows));
                    }
                } else {
                    // Unknown nested field — try to parse as array of string values
                    let nested_rows = parse_composite_form_data(&sub_form, &base_key, &[]);
                    obj.insert(base_key, serde_json::Value::Array(nested_rows));
                }
            }

            serde_json::Value::Object(obj)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldDefinition, FieldTab, LocalizedString, SelectOption};

    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, ft).build()
    }

    // --- parse_composite_form_data: flat (1-level) ---

    #[test]
    fn parse_flat_array_rows() {
        let mut form = HashMap::new();
        form.insert("slides[0][title]".to_string(), "First".to_string());
        form.insert("slides[0][caption]".to_string(), "Cap 1".to_string());
        form.insert("slides[1][title]".to_string(), "Second".to_string());
        form.insert("slides[1][caption]".to_string(), "Cap 2".to_string());

        let sub_defs = vec![
            make_field("title", FieldType::Text),
            make_field("caption", FieldType::Text),
        ];
        let result = parse_composite_form_data(&form, "slides", &sub_defs);

        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["title"], "First");
        assert_eq!(result[0]["caption"], "Cap 1");
        assert_eq!(result[1]["title"], "Second");
        assert_eq!(result[1]["caption"], "Cap 2");
    }

    #[test]
    fn parse_empty_form_returns_empty() {
        let form = HashMap::new();
        let result = parse_composite_form_data(&form, "items", &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_blocks_with_block_type() {
        let mut form = HashMap::new();
        form.insert("content[0][_block_type]".to_string(), "text".to_string());
        form.insert("content[0][body]".to_string(), "Hello".to_string());
        form.insert("content[1][_block_type]".to_string(), "image".to_string());
        form.insert("content[1][url]".to_string(), "/img.jpg".to_string());

        let result = parse_composite_form_data(&form, "content", &[]);

        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["_block_type"], "text");
        assert_eq!(result[0]["body"], "Hello");
        assert_eq!(result[1]["_block_type"], "image");
        assert_eq!(result[1]["url"], "/img.jpg");
    }

    // --- parse_composite_form_data: nested (2+ levels) ---

    #[test]
    fn parse_nested_array_in_blocks() {
        // content[0][_block_type] = gallery
        // content[0][title] = My Gallery
        // content[0][images][0][url] = img1.jpg
        // content[0][images][0][alt] = First
        // content[0][images][1][url] = img2.jpg
        // content[0][images][1][alt] = Second
        let mut form = HashMap::new();
        form.insert("content[0][_block_type]".to_string(), "gallery".to_string());
        form.insert("content[0][title]".to_string(), "My Gallery".to_string());
        form.insert(
            "content[0][images][0][url]".to_string(),
            "img1.jpg".to_string(),
        );
        form.insert(
            "content[0][images][0][alt]".to_string(),
            "First".to_string(),
        );
        form.insert(
            "content[0][images][1][url]".to_string(),
            "img2.jpg".to_string(),
        );
        form.insert(
            "content[0][images][1][alt]".to_string(),
            "Second".to_string(),
        );

        // Provide sub-field definitions so `images` is recognized as Array
        let mut images_field = make_field("images", FieldType::Array);
        images_field.fields = vec![
            make_field("url", FieldType::Text),
            make_field("alt", FieldType::Text),
        ];
        let sub_defs = vec![
            make_field("_block_type", FieldType::Text),
            make_field("title", FieldType::Text),
            images_field,
        ];

        let result = parse_composite_form_data(&form, "content", &sub_defs);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["_block_type"], "gallery");
        assert_eq!(result[0]["title"], "My Gallery");

        let images = result[0]["images"].as_array().unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0]["url"], "img1.jpg");
        assert_eq!(images[0]["alt"], "First");
        assert_eq!(images[1]["url"], "img2.jpg");
        assert_eq!(images[1]["alt"], "Second");
    }

    #[test]
    fn parse_nested_array_in_array() {
        // items[0][title] = Item 1
        // items[0][tags][0][name] = rust
        // items[0][tags][1][name] = web
        let mut form = HashMap::new();
        form.insert("items[0][title]".to_string(), "Item 1".to_string());
        form.insert("items[0][tags][0][name]".to_string(), "rust".to_string());
        form.insert("items[0][tags][1][name]".to_string(), "web".to_string());

        let mut tags_field = make_field("tags", FieldType::Array);
        tags_field.fields = vec![make_field("name", FieldType::Text)];
        let sub_defs = vec![make_field("title", FieldType::Text), tags_field];

        let result = parse_composite_form_data(&form, "items", &sub_defs);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["title"], "Item 1");

        let tags = result[0]["tags"].as_array().unwrap();
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0]["name"], "rust");
        assert_eq!(tags[1]["name"], "web");
    }

    #[test]
    fn parse_nested_group_in_array() {
        // entries[0][meta][0][author] = Alice
        // entries[0][meta][0][date] = 2026-01-01
        // (Group gets index [0] in form data since it's parsed as a single-element array)
        let mut form = HashMap::new();
        form.insert("entries[0][title]".to_string(), "Entry 1".to_string());
        form.insert(
            "entries[0][meta][0][author]".to_string(),
            "Alice".to_string(),
        );
        form.insert(
            "entries[0][meta][0][date]".to_string(),
            "2026-01-01".to_string(),
        );

        let mut meta_field = make_field("meta", FieldType::Group);
        meta_field.fields = vec![
            make_field("author", FieldType::Text),
            make_field("date", FieldType::Date),
        ];
        let sub_defs = vec![make_field("title", FieldType::Text), meta_field];

        let result = parse_composite_form_data(&form, "entries", &sub_defs);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["title"], "Entry 1");

        // Group should be an object, not an array
        let meta = &result[0]["meta"];
        assert!(
            meta.is_object(),
            "Group should be parsed as object, got: {:?}",
            meta
        );
        assert_eq!(meta["author"], "Alice");
        assert_eq!(meta["date"], "2026-01-01");
    }

    #[test]
    fn parse_3_level_nesting() {
        // page[0][sections][0][items][0][title] = Deep leaf
        let mut form = HashMap::new();
        form.insert(
            "page[0][sections][0][items][0][title]".to_string(),
            "Deep leaf".to_string(),
        );
        form.insert(
            "page[0][sections][0][name]".to_string(),
            "Section 1".to_string(),
        );
        form.insert("page[0][name]".to_string(), "Page 1".to_string());

        let mut items_field = make_field("items", FieldType::Array);
        items_field.fields = vec![make_field("title", FieldType::Text)];
        let mut sections_field = make_field("sections", FieldType::Array);
        sections_field.fields = vec![make_field("name", FieldType::Text), items_field];
        let sub_defs = vec![make_field("name", FieldType::Text), sections_field];

        let result = parse_composite_form_data(&form, "page", &sub_defs);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["name"], "Page 1");

        let sections = result[0]["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0]["name"], "Section 1");

        let items = sections[0]["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["title"], "Deep leaf");
    }

    // --- extract_join_data_from_form: integration ---

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

    // --- extract_join_data_from_form: layout field recursion (regression) ---

    #[test]
    fn extract_join_data_blocks_inside_tabs() {
        // Regression: blocks inside a Tabs field were silently dropped on save
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

    // --- transform_select_has_many tests ---

    #[test]
    fn transform_select_has_many_converts_comma_separated() {
        let mut form = HashMap::new();
        form.insert("tags".to_string(), "red,blue,green".to_string());

        let mut field = make_field("tags", FieldType::Select);
        field.has_many = true;
        field.options = vec![
            SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
            SelectOption::new(LocalizedString::Plain("Blue".to_string()), "blue"),
            SelectOption::new(LocalizedString::Plain("Green".to_string()), "green"),
        ];

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("tags").unwrap(), r#"["red","blue","green"]"#);
    }

    #[test]
    fn transform_select_has_many_empty_value() {
        let mut form = HashMap::new();
        form.insert("tags".to_string(), String::new());

        let mut field = make_field("tags", FieldType::Select);
        field.has_many = true;

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("tags").unwrap(), "[]");
    }

    #[test]
    fn transform_select_has_many_missing_key() {
        let mut form = HashMap::new();

        let mut field = make_field("tags", FieldType::Select);
        field.has_many = true;

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("tags").unwrap(), "[]");
    }

    #[test]
    fn transform_select_has_many_single_value() {
        let mut form = HashMap::new();
        form.insert("color".to_string(), "red".to_string());

        let mut field = make_field("color", FieldType::Select);
        field.has_many = true;

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("color").unwrap(), r#"["red"]"#);
    }

    #[test]
    fn transform_select_has_many_skips_non_has_many() {
        let mut form = HashMap::new();
        form.insert("status".to_string(), "active".to_string());

        let field = make_field("status", FieldType::Select);
        // has_many is false by default

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("status").unwrap(), "active"); // unchanged
    }

    #[test]
    fn transform_select_has_many_in_group() {
        let mut form = HashMap::new();
        form.insert("meta__tags".to_string(), "a,b".to_string());

        let mut tag_field = make_field("tags", FieldType::Select);
        tag_field.has_many = true;

        let mut group = make_field("meta", FieldType::Group);
        group.fields = vec![tag_field];

        transform_select_has_many(&mut form, &[group]);
        assert_eq!(form.get("meta__tags").unwrap(), r#"["a","b"]"#);
    }

    #[test]
    fn parse_array_with_tabs_sub_fields() {
        // Sub-fields are inside a Tabs wrapper but form keys are flat
        let mut form = HashMap::new();
        form.insert("items[0][title]".to_string(), "Hello".to_string());
        form.insert("items[0][body]".to_string(), "World".to_string());
        form.insert("items[1][title]".to_string(), "Second".to_string());
        form.insert("items[1][body]".to_string(), "Content".to_string());

        let sub_defs = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![
                    FieldTab::new("General", vec![make_field("title", FieldType::Text)]),
                    FieldTab::new("Content", vec![make_field("body", FieldType::Text)]),
                ])
                .build(),
        ];

        let result = parse_composite_form_data(&form, "items", &sub_defs);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["title"], "Hello");
        assert_eq!(result[0]["body"], "World");
        assert_eq!(result[1]["title"], "Second");
        assert_eq!(result[1]["body"], "Content");
    }

    #[test]
    fn parse_array_with_row_sub_fields() {
        let mut form = HashMap::new();
        form.insert("items[0][x]".to_string(), "10".to_string());
        form.insert("items[0][y]".to_string(), "20".to_string());

        let sub_defs = vec![
            FieldDefinition::builder("row_wrap", FieldType::Row)
                .fields(vec![
                    make_field("x", FieldType::Text),
                    make_field("y", FieldType::Text),
                ])
                .build(),
        ];

        let result = parse_composite_form_data(&form, "items", &sub_defs);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["x"], "10");
        assert_eq!(result[0]["y"], "20");
    }
}

/// Parse a multipart form request, extracting form fields and an optional file upload.
pub(crate) async fn parse_multipart_form(
    request: axum::extract::Request,
    state: &AdminState,
) -> Result<(HashMap<String, String>, Option<UploadedFile>), anyhow::Error> {
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse multipart: {}", e))?;

    let mut form_data = HashMap::new();
    let mut file: Option<UploadedFile> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read multipart field: {}", e))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "_file" && field.file_name().is_some() {
            let filename = field.file_name().unwrap_or("").to_string();
            let content_type = field
                .content_type()
                .unwrap_or("application/octet-stream")
                .to_string();
            let data = field
                .bytes()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to read file data: {}", e))?;
            if !data.is_empty() {
                file = Some(
                    UploadedFileBuilder::new(filename, content_type)
                        .data(data.to_vec())
                        .build(),
                );
            }
        } else {
            let text = field.text().await.unwrap_or_default();
            form_data.insert(name, text);
        }
    }

    Ok((form_data, file))
}
