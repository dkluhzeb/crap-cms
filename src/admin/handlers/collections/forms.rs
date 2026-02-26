//! Form parsing helpers: multipart, array fields, upload metadata.

use axum::extract::{FromRequest, Multipart};
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::core::field::FieldType;
use crate::core::upload::UploadedFile;

/// Extract join table data from form submission for has-many relationships and array fields.
/// Returns a map suitable for `query::save_join_table_data`.
pub(crate) fn extract_join_data_from_form(
    form: &HashMap<String, String>,
    field_defs: &[crate::core::field::FieldDefinition],
) -> HashMap<String, serde_json::Value> {
    let mut join_data = HashMap::new();

    for field in field_defs {
        match field.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        // Has-many: comma-separated IDs in form value
                        if let Some(val) = form.get(&field.name) {
                            join_data.insert(field.name.clone(), serde_json::Value::String(val.clone()));
                        } else {
                            // Empty selection — clear all
                            join_data.insert(field.name.clone(), serde_json::Value::String(String::new()));
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
            _ => {}
        }
    }

    join_data
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
    sub_field_defs: &[crate::core::field::FieldDefinition],
) -> Vec<serde_json::Value> {
    let prefix = format!("{}[", field_name);
    let mut rows: std::collections::BTreeMap<usize, Vec<(String, String)>> = std::collections::BTreeMap::new();

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
                                rows.entry(idx).or_default().push((sub_key.to_string(), value.clone()));
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

    rows.into_iter().map(|(_idx, entries)| {
        let mut obj = serde_json::Map::new();

        // Separate leaf entries from nested entries
        let mut nested_keys: HashMap<String, Vec<(String, String)>> = HashMap::new();

        for (key, value) in entries {
            if let Some(bracket_pos) = key.find('[') {
                // This is a nested key like "items[1][caption]"
                let base_key = &key[..bracket_pos];
                let rest = &key[bracket_pos..]; // "[1][caption]"
                nested_keys.entry(base_key.to_string())
                    .or_default()
                    .push((rest.to_string(), value));
            } else {
                // Simple leaf key
                obj.insert(key, serde_json::Value::String(value));
            }
        }

        // Process nested keys recursively
        for (base_key, nested_entries) in nested_keys {
            // Look up the field definition for this sub-field to determine type
            let sf_def = sub_field_defs.iter().find(|sf| sf.name == base_key);
            let nested_sub_defs = sf_def.map(|sf| sf.fields.as_slice()).unwrap_or(&[]);

            // Reconstruct a form-like HashMap with the base_key as prefix
            let mut sub_form = HashMap::new();
            for (rest, value) in &nested_entries {
                // rest is like "[1][caption]" — reconstruct full key as "base_key[1][caption]"
                sub_form.insert(format!("{}{}", base_key, rest), value.clone());
            }

            let is_composite = sf_def.map(|sf| matches!(
                sf.field_type,
                FieldType::Array | FieldType::Blocks | FieldType::Group
            )).unwrap_or(false);

            if is_composite {
                let nested_rows = parse_composite_form_data(&sub_form, &base_key, nested_sub_defs);
                // For group fields, the "rows" are actually a single object
                if sf_def.map(|sf| sf.field_type == FieldType::Group).unwrap_or(false) {
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
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldDefinition, FieldAdmin, FieldHooks, FieldAccess};

    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: ft,
            required: false,
            unique: false,
            validate: None,
            default_value: None,
            options: Vec::new(),
            admin: FieldAdmin::default(),
            hooks: FieldHooks::default(),
            access: FieldAccess::default(),
            relationship: None,
            fields: Vec::new(),
            blocks: Vec::new(),
            localized: false,
            picker_appearance: None,
        }
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
        form.insert("content[0][images][0][url]".to_string(), "img1.jpg".to_string());
        form.insert("content[0][images][0][alt]".to_string(), "First".to_string());
        form.insert("content[0][images][1][url]".to_string(), "img2.jpg".to_string());
        form.insert("content[0][images][1][alt]".to_string(), "Second".to_string());

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
        let sub_defs = vec![
            make_field("title", FieldType::Text),
            tags_field,
        ];

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
        form.insert("entries[0][meta][0][author]".to_string(), "Alice".to_string());
        form.insert("entries[0][meta][0][date]".to_string(), "2026-01-01".to_string());

        let mut meta_field = make_field("meta", FieldType::Group);
        meta_field.fields = vec![
            make_field("author", FieldType::Text),
            make_field("date", FieldType::Date),
        ];
        let sub_defs = vec![
            make_field("title", FieldType::Text),
            meta_field,
        ];

        let result = parse_composite_form_data(&form, "entries", &sub_defs);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["title"], "Entry 1");

        // Group should be an object, not an array
        let meta = &result[0]["meta"];
        assert!(meta.is_object(), "Group should be parsed as object, got: {:?}", meta);
        assert_eq!(meta["author"], "Alice");
        assert_eq!(meta["date"], "2026-01-01");
    }

    #[test]
    fn parse_3_level_nesting() {
        // page[0][sections][0][items][0][title] = Deep leaf
        let mut form = HashMap::new();
        form.insert("page[0][sections][0][items][0][title]".to_string(), "Deep leaf".to_string());
        form.insert("page[0][sections][0][name]".to_string(), "Section 1".to_string());
        form.insert("page[0][name]".to_string(), "Page 1".to_string());

        let mut items_field = make_field("items", FieldType::Array);
        items_field.fields = vec![make_field("title", FieldType::Text)];
        let mut sections_field = make_field("sections", FieldType::Array);
        sections_field.fields = vec![
            make_field("name", FieldType::Text),
            items_field,
        ];
        let sub_defs = vec![
            make_field("name", FieldType::Text),
            sections_field,
        ];

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
        slides_field.fields = vec![
            make_field("title", FieldType::Text),
            images_field,
        ];

        let result = extract_join_data_from_form(&form, &[slides_field]);
        let slides = result.get("slides").unwrap().as_array().unwrap();
        assert_eq!(slides.len(), 1);
        assert_eq!(slides[0]["title"], "Slide 1");

        let images = slides[0]["images"].as_array().unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0]["url"], "a.jpg");
        assert_eq!(images[1]["url"], "b.jpg");
    }
}

/// Parse a multipart form request, extracting form fields and an optional file upload.
pub(crate) async fn parse_multipart_form(
    request: axum::extract::Request,
    state: &AdminState,
) -> Result<(HashMap<String, String>, Option<UploadedFile>), anyhow::Error> {
    let mut multipart = Multipart::from_request(request, state).await
        .map_err(|e| anyhow::anyhow!("Failed to parse multipart: {}", e))?;

    let mut form_data = HashMap::new();
    let mut file: Option<UploadedFile> = None;

    while let Some(field) = multipart.next_field().await
        .map_err(|e| anyhow::anyhow!("Failed to read multipart field: {}", e))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "_file" && field.file_name().is_some() {
            let filename = field.file_name().unwrap_or("").to_string();
            let content_type = field.content_type()
                .unwrap_or("application/octet-stream").to_string();
            let data = field.bytes().await
                .map_err(|e| anyhow::anyhow!("Failed to read file data: {}", e))?;
            if !data.is_empty() {
                file = Some(UploadedFile {
                    filename,
                    content_type,
                    data: data.to_vec(),
                });
            }
        } else {
            let text = field.text().await.unwrap_or_default();
            form_data.insert(name, text);
        }
    }

    Ok((form_data, file))
}


