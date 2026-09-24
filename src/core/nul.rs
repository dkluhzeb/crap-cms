//! NUL-character rejection for written document values.
//!
//! Postgres cannot store a NUL in a `TEXT` column, and JSON text that carries
//! the `\u0000` escape makes every `::jsonb` cast over it fail — a single stored
//! NUL inside an array/blocks row would turn every row-path filter on its table
//! (and the drafted-file lookup) into an error for every caller. So every string
//! a write carries is refused when it holds a NUL, at any depth: top-level
//! columns, group sub-fields, array/blocks rows at any nesting, has-many lists,
//! and the decoded content of JSON-bearing values (`json` and `richtext` fields,
//! and a container or has-many value sent as JSON text) — object keys included.
//! The rule holds on both backends so stored content stays portable.
//!
//! Errors carry the same field keys the other validation checks use: the flat
//! `group__sub` column name at the top level, `field[idx][sub]` inside a row.

use serde_json::{Map, Value};

use crate::core::{
    BLOCK_TYPE_KEY, FieldChildren, FieldDefinition, FieldType, JsonRoot, field_children,
    find_field,
    validate::{FieldError, ValidationError},
};

/// Translation key of the error a NUL-carrying value produces.
const NUL_KEY: &str = "validation.nul_character";

/// Every NUL-carrying value in `data` (the canonical nested document shape),
/// one error per offending field.
///
/// `locales` are the configured locale codes; a localized field given as an
/// object keyed by exactly those codes (the export/import shape) is checked
/// per locale. Pass an empty slice where values are always single-locale.
pub fn nul_character_errors(
    data: &dyn JsonRoot,
    fields: &[FieldDefinition],
    locales: &[String],
) -> Vec<FieldError> {
    let mut scan = NulScan {
        locales,
        errors: Vec::new(),
    };

    let mut keys = data.root_keys();
    keys.sort();

    for key in &keys {
        if let Some(value) = data.root_get(key) {
            scan.entry(fields, &Level::root(), key, value);
        }
    }

    scan.errors
}

/// Refuse `data` when any value in it carries a NUL — the gate on the final
/// data a write persists, after every hook has run. Values are single-locale
/// there, so no locale map is recognised.
///
/// # Errors
///
/// Returns a [`ValidationError`] naming every offending field.
pub fn reject_nul_characters(
    data: &dyn JsonRoot,
    fields: &[FieldDefinition],
) -> Result<(), ValidationError> {
    let errors = nul_character_errors(data, fields, &[]);

    if errors.is_empty() {
        return Ok(());
    }

    Err(ValidationError::new(errors))
}

/// Where a level's keys sit: the document root or one array/blocks row, plus
/// the `group__` prefix accumulated by the groups entered since.
struct Level<'p> {
    row: Option<(&'p str, usize)>,
    group_prefix: String,
}

impl<'p> Level<'p> {
    fn root() -> Self {
        Self {
            row: None,
            group_prefix: String::new(),
        }
    }

    fn row(parent: &'p str, idx: usize) -> Self {
        Self {
            row: Some((parent, idx)),
            group_prefix: String::new(),
        }
    }

    /// The level inside group `key` of this level.
    fn group(&self, key: &str) -> Self {
        Self {
            row: self.row,
            group_prefix: format!("{}{key}__", self.group_prefix),
        }
    }

    /// The error key of `key` at this level.
    fn path(&self, key: &str) -> String {
        let key = key.replace('\0', "\\0");

        match self.row {
            None => format!("{}{key}", self.group_prefix),
            Some((parent, idx)) => format!("{parent}[{idx}][{}{key}]", self.group_prefix),
        }
    }
}

struct NulScan<'a> {
    locales: &'a [String],
    errors: Vec<FieldError>,
}

impl NulScan<'_> {
    /// One key/value pair of an object level, matched to its field by name.
    fn entry(&mut self, fields: &[FieldDefinition], level: &Level<'_>, key: &str, value: &Value) {
        if key.contains('\0') {
            self.reject(key, level.path(key));

            return;
        }

        let field = find_field(key, fields).or_else(|| find_flat_group_child(key, fields));

        self.value(field, value, level, key);
    }

    fn value(
        &mut self,
        field: Option<&FieldDefinition>,
        value: &Value,
        level: &Level<'_>,
        key: &str,
    ) {
        let Some(field) = field else {
            self.generic(value, key, level.path(key));

            return;
        };

        if let Some(by_locale) = self.locale_map(field, value) {
            for localized in by_locale.values() {
                self.value(Some(field), localized, level, key);
            }

            return;
        }

        match (field_children(field), value) {
            (FieldChildren::Group(subs), Value::Object(obj)) => {
                let inner = level.group(key);

                for (sub_key, sub_value) in obj {
                    self.entry(subs, &inner, sub_key, sub_value);
                }
            }
            (FieldChildren::Array(_) | FieldChildren::Blocks(_), Value::Array(rows)) => {
                self.rows(field, rows, &level.path(key));
            }
            (_, Value::String(s)) => self.string(field, s, level, key),
            _ => self.generic(value, &field.name, level.path(key)),
        }
    }

    /// Each row of an array/blocks value, its keys matched to the row's
    /// sub-fields (a block's by its `_block_type`).
    fn rows(&mut self, field: &FieldDefinition, rows: &[Value], path: &str) {
        for (idx, row) in rows.iter().enumerate() {
            let Value::Object(obj) = row else {
                self.generic(row, &field.name, format!("{path}[{idx}]"));

                continue;
            };

            let level = Level::row(path, idx);
            let subs = row_sub_fields(field, obj);

            for (sub_key, sub_value) in obj {
                self.entry(subs, &level, sub_key, sub_value);
            }
        }
    }

    /// A string value: refused when it holds a NUL; JSON-bearing values are
    /// decoded and their content checked too.
    fn string(&mut self, field: &FieldDefinition, s: &str, level: &Level<'_>, key: &str) {
        if s.contains('\0') {
            self.reject(&field.name, level.path(key));

            return;
        }

        let opaque_json =
            matches!(field.field_type, FieldType::Json | FieldType::Richtext) || field.has_many;
        let structured = field.field_type.has_rows() || field.field_type == FieldType::Group;

        if !opaque_json && !structured {
            return;
        }

        let Ok(decoded) = serde_json::from_str::<Value>(s) else {
            return;
        };

        if opaque_json {
            self.generic(&decoded, &field.name, level.path(key));
        } else if decoded.is_array() || decoded.is_object() {
            self.value(Some(field), &decoded, level, key);
        }
    }

    /// A value with no further schema: any NUL in a string or key rejects it.
    fn generic(&mut self, value: &Value, name: &str, path: String) {
        if holds_nul(value) {
            self.reject(name, path);
        }
    }

    /// A localized value given per locale: an object keyed by configured
    /// locale codes only.
    fn locale_map<'v>(
        &self,
        field: &FieldDefinition,
        value: &'v Value,
    ) -> Option<&'v Map<String, Value>> {
        if self.locales.is_empty() || !field.localized {
            return None;
        }

        let Value::Object(obj) = value else {
            return None;
        };

        let by_locale = !obj.is_empty() && obj.keys().all(|k| self.locales.contains(k));

        by_locale.then_some(obj)
    }

    fn reject(&mut self, name: &str, path: String) {
        let name = name.replace('\0', "\\0");

        self.errors.push(
            FieldError::with_key(
                path,
                format!("{name} must not contain NUL characters"),
                NUL_KEY,
            )
            .with_param("field", name),
        );
    }
}

/// The sub-fields one array/blocks row is matched against — none for a block
/// of an undeclared type, whose keys are then checked schema-free.
fn row_sub_fields<'f>(
    field: &'f FieldDefinition,
    row: &Map<String, Value>,
) -> &'f [FieldDefinition] {
    if field.field_type != FieldType::Blocks {
        return &field.fields;
    }

    let block_type = row.get(BLOCK_TYPE_KEY).and_then(Value::as_str);

    let Some(block) = field
        .blocks
        .iter()
        .find(|b| Some(b.block_type.as_str()) == block_type)
    else {
        return &[];
    };

    &block.fields
}

/// A group sub-field addressed by its flat `group__sub` column name.
fn find_flat_group_child<'f>(
    key: &str,
    fields: &'f [FieldDefinition],
) -> Option<&'f FieldDefinition> {
    let (group, rest) = key.split_once("__")?;
    let group = find_field(group, fields).filter(|f| f.field_type == FieldType::Group)?;

    find_field(rest, &group.fields).or_else(|| find_flat_group_child(rest, &group.fields))
}

/// Whether any string or object key anywhere in `value` holds a NUL.
fn holds_nul(value: &Value) -> bool {
    match value {
        Value::String(s) => s.contains('\0'),
        Value::Array(items) => items.iter().any(holds_nul),
        Value::Object(obj) => obj.iter().any(|(k, v)| k.contains('\0') || holds_nul(v)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::{BlockDefinition, DocumentFields};

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn doc(value: Value) -> DocumentFields {
        let Value::Object(map) = value else {
            panic!("object");
        };

        map.into_iter().collect()
    }

    fn fields() -> Vec<FieldDefinition> {
        vec![
            text("title"),
            FieldDefinition::builder("body", FieldType::Code).build(),
            FieldDefinition::builder("meta", FieldType::Json).build(),
            FieldDefinition::builder("content", FieldType::Richtext).build(),
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    text("title"),
                    FieldDefinition::builder("extra", FieldType::Group)
                        .fields(vec![text("note")])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    text("label"),
                    FieldDefinition::builder("info", FieldType::Group)
                        .fields(vec![text("caption")])
                        .build(),
                    FieldDefinition::builder("children", FieldType::Array)
                        .fields(vec![text("name")])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("layout", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "hero",
                    vec![
                        text("heading"),
                        FieldDefinition::builder("data", FieldType::Json).build(),
                    ],
                )])
                .build(),
        ]
    }

    fn paths(value: Value) -> Vec<String> {
        nul_character_errors(&doc(value), &fields(), &[])
            .into_iter()
            .map(|e| e.field)
            .collect()
    }

    #[test]
    fn clean_values_pass() {
        let clean = json!({
            "title": "a",
            "body": "fn main() {}",
            "meta": "{\"k\": \"v\"}",
            "content": "<p>x</p>",
            "tags": ["a", "b"],
            "seo": { "title": "t", "extra": { "note": "n" } },
            "items": [{ "label": "l", "info": { "caption": "c" }, "children": [{ "name": "x" }] }],
            "layout": [{ "_block_type": "hero", "heading": "h", "data": { "k": [1, "v"] } }],
        });

        assert!(paths(clean).is_empty());
    }

    #[test]
    fn top_level_scalars_are_rejected() {
        assert_eq!(paths(json!({ "title": "a\0b" })), vec!["title"]);
        assert_eq!(paths(json!({ "body": "x\0" })), vec!["body"]);
    }

    /// The `\u0000` escape inside a JSON-bearing value decodes to a NUL — the
    /// text a `::jsonb` cast rejects.
    #[test]
    fn decoded_json_content_is_checked() {
        assert_eq!(
            paths(json!({ "meta": "{\"k\": \"\\u0000\"}" })),
            vec!["meta"]
        );
        assert_eq!(paths(json!({ "meta": { "k\0": 1 } })), vec!["meta"]);
        assert_eq!(paths(json!({ "meta": { "k": ["\0"] } })), vec!["meta"]);
        assert_eq!(
            paths(json!({ "content": "{\"type\":\"doc\",\"content\":[{\"text\":\"\\u0000\"}]}" })),
            vec!["content"]
        );
        assert_eq!(
            paths(json!({ "tags": "[\"a\", \"\\u0000\"]" })),
            vec!["tags"]
        );
    }

    /// A plain text value holding the six characters `\u0000` is not a NUL —
    /// it is stored (and JSON-encoded) as ordinary text.
    #[test]
    fn escape_text_in_a_text_field_is_not_a_nul() {
        assert!(paths(json!({ "title": "\\u0000", "body": "\"\\u0000\"" })).is_empty());
    }

    #[test]
    fn has_many_elements_are_rejected() {
        assert_eq!(paths(json!({ "tags": ["ok", "b\0"] })), vec!["tags"]);
    }

    #[test]
    fn group_sub_fields_report_their_column() {
        assert_eq!(
            paths(json!({ "seo": { "title": "\0" } })),
            vec!["seo__title"]
        );
        assert_eq!(
            paths(json!({ "seo": { "extra": { "note": "\0" } } })),
            vec!["seo__extra__note"]
        );
        assert_eq!(paths(json!({ "seo__title": "\0" })), vec!["seo__title"]);
    }

    #[test]
    fn row_values_report_their_row_path() {
        assert_eq!(
            paths(json!({ "items": [{ "label": "ok" }, { "label": "\0" }] })),
            vec!["items[1][label]"]
        );
        assert_eq!(
            paths(json!({ "items": [{ "info": { "caption": "\0" } }] })),
            vec!["items[0][info__caption]"]
        );
        assert_eq!(
            paths(json!({ "items": [{ "children": [{ "name": "\0" }] }] })),
            vec!["items[0][children][0][name]"]
        );
        assert_eq!(
            paths(json!({ "layout": [{ "_block_type": "hero", "heading": "\0" }] })),
            vec!["layout[0][heading]"]
        );
        assert_eq!(
            paths(json!({ "layout": [{ "_block_type": "hero", "data": { "x": "\0" } }] })),
            vec!["layout[0][data]"]
        );
    }

    /// A key a row does not declare is still stored in a block's JSON, so it is
    /// checked too — its name and its value.
    #[test]
    fn undeclared_keys_and_block_types_are_checked() {
        assert_eq!(
            paths(json!({ "layout": [{ "_block_type": "other", "x": { "y": "\0" } }] })),
            vec!["layout[0][x]"]
        );
        assert_eq!(
            paths(json!({ "items": [{ "a\0b": 1 }] })),
            vec!["items[0][a\\0b]"]
        );
        assert_eq!(paths(json!({ "unknown": ["\0"] })), vec!["unknown"]);
    }

    #[test]
    fn a_container_sent_as_json_text_is_checked() {
        assert_eq!(
            paths(json!({ "items": "[{\"label\": \"\\u0000\"}]" })),
            vec!["items[0][label]"]
        );
    }

    #[test]
    fn localized_values_per_locale_are_checked_with_locales() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Json)
                .localized(true)
                .build(),
        ];
        let data = doc(json!({ "meta": { "en": "{}", "de": "{\"k\": \"\\u0000\"}" } }));
        let locales = vec!["en".to_string(), "de".to_string()];

        let errors = nul_character_errors(&data, &fields, &locales);

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "meta");
        assert_eq!(errors[0].key.as_deref(), Some(NUL_KEY));
    }
}
