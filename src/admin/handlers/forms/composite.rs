//! Composite/indexed row parsing for nested form data (arrays, blocks, groups).

use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};

use crate::core::{BLOCK_TYPE_KEY, BlockDefinition, FieldDefinition, FieldType};

use crate::core::field::flatten_array_sub_fields;

use super::select_has_many::canonical_json_array;
/// Collect form entries into indexed rows, splitting each key into sub-key + value.
fn collect_indexed_rows(
    form: &HashMap<String, String>,
    prefix: &str,
) -> BTreeMap<usize, Vec<(String, String)>> {
    let mut rows: BTreeMap<usize, Vec<(String, String)>> = BTreeMap::new();

    for (key, value) in form {
        let Some(rest) = key.strip_prefix(prefix) else {
            continue;
        };

        if let Some((idx_str, after)) = rest.split_once(']')
            && let Ok(idx) = idx_str.parse::<usize>()
            && let Some(remaining) = after.strip_prefix('[')
            && let Some((sub_key, tail)) = remaining.split_once(']')
        {
            let entry_key = if tail.is_empty() {
                sub_key.to_string()
            } else {
                format!("{sub_key}{tail}")
            };

            rows.entry(idx)
                .or_default()
                .push((entry_key, value.clone()));
        }
    }

    rows
}

/// Nested entries grouped by base key, each with remaining bracket suffix + value.
type NestedEntries = HashMap<String, Vec<(String, String)>>;

/// Separate row entries into leaf values (flat keys) and nested groups (keys with brackets).
fn partition_entries(entries: Vec<(String, String)>) -> (Map<String, Value>, NestedEntries) {
    let mut leaves = Map::new();
    let mut nested: HashMap<String, Vec<(String, String)>> = HashMap::new();

    for (key, value) in entries {
        if let Some((base_key, rest_after_bracket)) = key.split_once('[') {
            let rest = format!("[{rest_after_bracket}");
            nested
                .entry(base_key.to_string())
                .or_default()
                .push((rest, value));
        } else {
            leaves.insert(key, Value::String(value));
        }
    }

    (leaves, nested)
}

/// Resolve a nested key group into a JSON value by recursively parsing composite form data.
fn resolve_nested_key(
    base_key: &str,
    nested_entries: &[(String, String)],
    flat_defs: &[&FieldDefinition],
) -> Value {
    let sf_def = flat_defs.iter().find(|sf| sf.name == base_key).copied();

    let sub_form: HashMap<String, String> = nested_entries
        .iter()
        .map(|(rest, value)| (format!("{base_key}{rest}"), value.clone()))
        .collect();

    // A nested Blocks sub-field is heterogeneous: dispatch each row on its own
    // `_block_type` (its field defs live in `blocks`, not `fields`).
    if let Some(sf) = sf_def
        && sf.field_type == FieldType::Blocks
    {
        return Value::Array(parse_blocks_form_data(&sub_form, base_key, &sf.blocks));
    }

    let nested_sub_defs = sf_def.map_or(&[][..], |sf| sf.fields.as_slice());
    let nested_rows = parse_composite_form_data(&sub_form, base_key, nested_sub_defs);

    let is_single_object = sf_def.is_some_and(|sf| {
        matches!(
            sf.field_type,
            FieldType::Group | FieldType::Row | FieldType::Collapsible | FieldType::Tabs
        )
    });

    if is_single_object {
        nested_rows
            .into_iter()
            .next()
            .unwrap_or(Value::Object(Map::new()))
    } else {
        Value::Array(nested_rows)
    }
}

/// Parse one indexed row into a JSON object, resolving each nested composite
/// key against `flat_defs` (the flattened sub-field defs for that row).
fn parse_row(entries: Vec<(String, String)>, flat_defs: &[&FieldDefinition]) -> Value {
    let (mut obj, nested_keys) = partition_entries(entries);

    // Normalize `has_many` scalar leaves (select/radio/text/number) into a
    // canonical JSON-array string — the same shape top-level `has_many` fields
    // use. The top-level normalizer doesn't descend into array/blocks rows, so
    // without this a nested multi-value field stays a collapsed `"a,b"` string
    // the renderer can't parse (`from_str` fails → empty selection on reload).
    for def in flat_defs {
        if !def.has_many
            || !matches!(
                def.field_type,
                FieldType::Select | FieldType::Radio | FieldType::Text | FieldType::Number
            )
        {
            continue;
        }

        if let Some(Value::String(raw)) = obj.get(&def.name) {
            let canonical = canonical_json_array(raw);
            obj.insert(def.name.clone(), Value::String(canonical));
        }
    }

    for (base_key, nested_entries) in nested_keys {
        let value = resolve_nested_key(&base_key, &nested_entries, flat_defs);
        obj.insert(base_key, value);
    }

    Value::Object(obj)
}

/// Recursively parse composite form data from flat form keys.
///
/// Handles arbitrarily nested keys like `content[0][items][1][title]`.
/// Uses field definitions to know which sub-fields are composites (need recursion)
/// vs. scalars (leaf values stored as strings).
///
/// Returns a Vec of JSON objects, one per row.
pub(crate) fn parse_composite_form_data(
    form: &HashMap<String, String>,
    field_name: &str,
    sub_field_defs: &[FieldDefinition],
) -> Vec<Value> {
    let prefix = format!("{field_name}[");
    let rows = collect_indexed_rows(form, &prefix);
    let flat_defs = flatten_array_sub_fields(sub_field_defs);

    rows.into_values()
        .map(|entries| parse_row(entries, &flat_defs))
        .collect()
}

/// Parse a **Blocks** field's rows. Unlike arrays, block rows are heterogeneous:
/// each row's sub-field defs are selected by its `_block_type` value (each block
/// type carries its own `fields`). Without this, a Group inside a block row can't
/// be recognized as a single-object composite and gets stored as a one-element
/// array instead of an object — invisible on render and lost on the next save.
pub(crate) fn parse_blocks_form_data(
    form: &HashMap<String, String>,
    field_name: &str,
    blocks: &[BlockDefinition],
) -> Vec<Value> {
    let prefix = format!("{field_name}[");
    let rows = collect_indexed_rows(form, &prefix);

    rows.into_values()
        .map(|entries| {
            // The row's `_block_type` leaf selects which block's fields apply.
            let block_fields = entries
                .iter()
                .find(|(k, _)| k == BLOCK_TYPE_KEY)
                .and_then(|(_, bt)| blocks.iter().find(|b| &b.block_type == bt))
                .map_or(&[][..], |b| b.fields.as_slice());
            let flat_defs = flatten_array_sub_fields(block_fields);

            parse_row(entries, &flat_defs)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldDefinition, FieldTab, FieldType};
    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, ft).build()
    }

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

    #[test]
    fn parse_nested_array_in_blocks() {
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

    /// Regression: a Group nested inside a Blocks row must store as an OBJECT,
    /// not a one-element array. The blocks parser resolves the row's sub-field
    /// defs from its `_block_type`; previously it ran with empty defs (from the
    /// `&[]` call in `join_data`), so the group wasn't recognized as a single
    /// object and its data was lost on render / re-save.
    #[test]
    fn parse_group_in_blocks_stores_as_object() {
        use crate::core::BlockDefinition;

        // A group inside a block row is submitted with a `[0]` index on the
        // group name (how the enrich path names group children).
        let mut form = HashMap::new();
        form.insert("content[0][_block_type]".to_string(), "hero".to_string());
        form.insert("content[0][title]".to_string(), "Welcome".to_string());
        form.insert(
            "content[0][meta][0][author]".to_string(),
            "Alice".to_string(),
        );
        form.insert("content[0][meta][0][year]".to_string(), "2026".to_string());

        let mut meta = make_field("meta", FieldType::Group);
        meta.fields = vec![
            make_field("author", FieldType::Text),
            make_field("year", FieldType::Text),
        ];
        let hero = BlockDefinition::new("hero", vec![make_field("title", FieldType::Text), meta]);

        let result = parse_blocks_form_data(&form, "content", &[hero]);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["_block_type"], "hero");
        assert_eq!(result[0]["title"], "Welcome");
        // The group must be an object, not `[{...}]`.
        let meta_val = &result[0]["meta"];
        assert!(
            meta_val.is_object(),
            "group must serialize as object, got {meta_val}"
        );
        assert_eq!(meta_val["author"], "Alice");
        assert_eq!(meta_val["year"], "2026");
    }

    /// Regression: a `has_many` select nested in an array row is normalized to
    /// a canonical JSON-array string (like top-level `has_many`), so it round-trips
    /// instead of staying a collapsed `"a,b"` the renderer can't parse.
    #[test]
    fn has_many_select_in_array_row_normalizes_to_json_array() {
        let mut form = HashMap::new();
        form.insert("items[0][title]".to_string(), "T".to_string());
        form.insert("items[0][tags]".to_string(), "a,b,c".to_string());

        let mut tags = make_field("tags", FieldType::Select);
        tags.has_many = true;
        let sub_defs = vec![make_field("title", FieldType::Text), tags];

        let result = parse_composite_form_data(&form, "items", &sub_defs);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["title"], "T");
        assert_eq!(result[0]["tags"], r#"["a","b","c"]"#);
    }

    /// An unknown `_block_type` (or none) falls back to empty defs — the row
    /// still parses its scalar leaves without panicking.
    #[test]
    fn parse_blocks_unknown_type_keeps_scalars() {
        use crate::core::BlockDefinition;

        let mut form = HashMap::new();
        form.insert("content[0][_block_type]".to_string(), "mystery".to_string());
        form.insert("content[0][body]".to_string(), "text".to_string());

        let known = BlockDefinition::new("hero", vec![make_field("title", FieldType::Text)]);
        let result = parse_blocks_form_data(&form, "content", &[known]);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["_block_type"], "mystery");
        assert_eq!(result[0]["body"], "text");
    }

    #[test]
    fn parse_nested_array_in_array() {
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

        let meta = &result[0]["meta"];
        assert!(
            meta.is_object(),
            "Group should be parsed as object, got: {meta:?}"
        );
        assert_eq!(meta["author"], "Alice");
        assert_eq!(meta["date"], "2026-01-01");
    }

    #[test]
    fn parse_3_level_nesting() {
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

    #[test]
    fn parse_array_with_tabs_sub_fields() {
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
