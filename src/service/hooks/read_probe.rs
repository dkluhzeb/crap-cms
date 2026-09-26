//! A stored document shaped for judging which of its values a reader may see.
//!
//! A field-read strip removes what its rules deny, but a value the document
//! does not carry cannot be removed, so a rule on an empty value would never be
//! seen to deny. The probe gives every declared field a key — `null` for a
//! value the document does not carry — at every depth, so comparing the probe
//! with its stripped copy names every value the reader cannot see, empty ones
//! included.
//!
//! A row the document does not hold yet — one a write adds, or the edit form's
//! template for new rows — has nothing stored to judge. The probe can stand one
//! in: an empty template row appended to a list (its `id` `null`, one per block
//! type for a blocks field), judged like any stored row. Whatever a rule denies
//! on that empty row, a new row may not carry and the form does not offer.

use serde_json::{Map, Value};

use crate::core::{
    BLOCK_TYPE_KEY, BlockDefinition, FieldChildren, FieldDefinition, field_children,
};

/// Which lists of the probe get an empty template row.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TemplateRows {
    /// Only the document's own lists (at the top level or in a group), whose
    /// rows have an identity a write matches them by.
    TopLevel,
    /// Every list, at every depth — the edit form offers a new row everywhere.
    EveryDepth,
}

/// The sub-field schema of a list's rows.
#[derive(Clone, Copy)]
pub(crate) enum RowSchema<'f> {
    Array(&'f [FieldDefinition]),
    Blocks(&'f [BlockDefinition]),
}

impl<'f> RowSchema<'f> {
    /// The row schema of `field`, when it is an array or blocks field.
    pub(crate) fn of(field: &'f FieldDefinition) -> Option<Self> {
        match field_children(field) {
            FieldChildren::Array(sub) => Some(Self::Array(sub)),
            FieldChildren::Blocks(blocks) => Some(Self::Blocks(blocks)),
            _ => None,
        }
    }

    /// The fields of one row: the array's sub-fields, or the fields of the
    /// block the row's `_block_type` names.
    pub(crate) fn fields_of(self, row: &Map<String, Value>) -> Option<&'f [FieldDefinition]> {
        match self {
            Self::Array(fields) => Some(fields),
            Self::Blocks(blocks) => {
                let block_type = row.get(BLOCK_TYPE_KEY).and_then(Value::as_str)?;

                blocks
                    .iter()
                    .find(|block| block.block_type == block_type)
                    .map(|block| block.fields.as_slice())
            }
        }
    }

    /// The empty template rows of this list: one for an array, one per block
    /// type for a blocks field.
    fn templates(self) -> Vec<Map<String, Value>> {
        let template = |block_type: Option<&str>| {
            let mut row = Map::new();
            row.insert("id".to_string(), Value::Null);

            if let Some(block_type) = block_type {
                row.insert(BLOCK_TYPE_KEY.to_string(), Value::from(block_type));
            }

            row
        };

        match self {
            Self::Array(_) => vec![template(None)],
            Self::Blocks(blocks) => blocks
                .iter()
                .map(|block| template(Some(block.block_type.as_str())))
                .collect(),
        }
    }
}

/// Whether `row` is a template row the probe appended: its `id` is present and
/// `null`. A stored row always carries its id.
#[must_use]
pub(crate) fn is_template_row(row: &Map<String, Value>) -> bool {
    matches!(row.get("id"), Some(Value::Null))
}

/// The template row of `rows` that stands for a new row shaped like `row`: the
/// one of the same block type.
#[must_use]
pub(crate) fn template_row_index(rows: &[Value], row: &Map<String, Value>) -> Option<usize> {
    rows.iter().position(|candidate| {
        candidate.as_object().is_some_and(|candidate| {
            is_template_row(candidate) && candidate.get(BLOCK_TYPE_KEY) == row.get(BLOCK_TYPE_KEY)
        })
    })
}

/// Shape the document-level `level` as a probe: every declared field keyed at
/// every depth, and the template rows `templates` asks for.
pub(crate) fn mark_absent(
    fields: &[FieldDefinition],
    level: &mut Map<String, Value>,
    templates: TemplateRows,
) {
    mark_level(fields, level, true, templates);
}

/// [`mark_absent`] for one level; `with_templates` decides whether the lists
/// at this level get template rows.
fn mark_level(
    fields: &[FieldDefinition],
    level: &mut Map<String, Value>,
    with_templates: bool,
    templates: TemplateRows,
) {
    for field in fields {
        match field_children(field) {
            FieldChildren::Wrapper(sub) => mark_level(sub, level, with_templates, templates),
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    mark_level(&tab.fields, level, with_templates, templates);
                }
            }
            FieldChildren::Group(sub) => {
                let group = level
                    .entry(field.name.clone())
                    .or_insert_with(|| Value::Object(Map::new()));

                if let Value::Object(inner) = group {
                    mark_level(sub, inner, with_templates, templates);
                }
            }
            FieldChildren::Array(sub) => {
                let list = List::new(&field.name, RowSchema::Array(sub));
                mark_rows(level, &list, with_templates, templates);
            }
            FieldChildren::Blocks(blocks) => {
                let list = List::new(&field.name, RowSchema::Blocks(blocks));
                mark_rows(level, &list, with_templates, templates);
            }
            FieldChildren::Leaf => {
                for column in field.columns_with_companions(&field.name) {
                    level.entry(column).or_insert(Value::Null);
                }
            }
        }
    }
}

/// A list field as the probe reaches it: its key and its row schema.
struct List<'a> {
    name: &'a str,
    schema: RowSchema<'a>,
}

impl<'a> List<'a> {
    fn new(name: &'a str, schema: RowSchema<'a>) -> Self {
        Self { name, schema }
    }
}

/// [`mark_level`] for a list: the list itself, its template rows when asked
/// for, then each of its rows.
fn mark_rows(
    level: &mut Map<String, Value>,
    list: &List<'_>,
    with_templates: bool,
    templates: TemplateRows,
) {
    let value = level.entry(list.name.to_string()).or_insert(Value::Null);

    if with_templates && value.is_null() {
        *value = Value::Array(Vec::new());
    }

    let Value::Array(rows) = value else {
        return;
    };

    if with_templates {
        rows.extend(list.schema.templates().into_iter().map(Value::Object));
    }

    let nested = templates == TemplateRows::EveryDepth;

    for row in rows.iter_mut() {
        let Value::Object(row) = row else {
            continue;
        };

        if let Some(fields) = list.schema.fields_of(row) {
            mark_level(fields, row, nested, templates);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::FieldType;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!("fixture is an object"),
        }
    }

    fn schema() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                    FieldDefinition::builder("inner", FieldType::Array)
                        .fields(vec![FieldDefinition::builder("x", FieldType::Text).build()])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("body", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "quote",
                    vec![FieldDefinition::builder("text", FieldType::Text).build()],
                )])
                .build(),
        ]
    }

    /// Every declared field gets a key, and each document-level list gets its
    /// template rows — one per block type — while a list inside a row does not.
    #[test]
    fn a_top_level_probe_marks_every_field_and_templates_the_document_lists() {
        let mut level = object(json!({ "items": [{ "id": "r1", "label": "a" }] }));

        mark_absent(&schema(), &mut level, TemplateRows::TopLevel);

        assert_eq!(
            Value::Object(level),
            json!({
                "title": null,
                "items": [
                    { "id": "r1", "label": "a", "inner": null },
                    { "id": null, "label": null, "inner": null },
                ],
                "body": [{ "id": null, "_block_type": "quote", "text": null }],
            })
        );
    }

    /// The edit form offers a new row in every list, so its probe templates
    /// the lists inside rows too — the template rows' own lists included.
    #[test]
    fn an_every_depth_probe_templates_the_lists_inside_rows() {
        let mut level = object(json!({ "items": [{ "id": "r1", "inner": [{ "x": "1" }] }] }));

        mark_absent(&schema(), &mut level, TemplateRows::EveryDepth);

        assert_eq!(
            level["items"][0]["inner"],
            json!([{ "x": "1" }, { "id": null, "x": null }])
        );
        assert_eq!(
            level["items"][1]["inner"],
            json!([{ "id": null, "x": null }])
        );
    }

    #[test]
    fn a_template_row_is_found_by_its_block_type() {
        let rows = vec![
            json!({ "id": "b1", "_block_type": "quote" }),
            json!({ "id": null, "_block_type": "image" }),
            json!({ "id": null, "_block_type": "quote" }),
        ];
        let new_row = object(json!({ "_block_type": "quote", "text": "t" }));

        assert_eq!(template_row_index(&rows, &new_row), Some(2));
        assert!(!is_template_row(&object(rows[0].clone())));
    }
}
