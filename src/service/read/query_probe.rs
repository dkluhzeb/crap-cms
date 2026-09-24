//! Container-shaped probe documents for the filter/sort readability check.
//!
//! A query path names a value inside the document: `seo__title` (or
//! `seo.title`) inside a group, `items.secret` inside an array's rows,
//! `content.body` inside a block row, `items.inner.x` inside a nested array.
//! The field-read strip only reaches a nested field's `access.read` rule when
//! the document holds the containers on the way to it, so a probe carrying
//! just the root key would never judge the nested rule. This module builds the
//! probe in the path's real shape — groups as objects, arrays and blocks as a
//! single row — so the same strip responses use evaluates every field on the
//! path, and reports whether the leaf survived.
//!
//! A path does not name the block type of a block row, so a block step fans
//! out into one shape per block type that holds the next field: the path is
//! readable only when its leaf survives in every shape.

use serde_json::{Map, Value};

use crate::core::{
    BLOCK_TYPE_KEY, BlockDefinition, Document, FieldChildren, FieldDefinition, field_children,
    flatten_array_sub_fields,
};

/// One step from a document down to a query path's leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProbeStep {
    /// The key at the current object level.
    Key(String),
    /// Into the single row of the rows value at the current position. A block
    /// row carries its block type, so the strip judges that block's fields.
    Row(Option<String>),
}

/// Every shape `path` takes in a document of `fields`, one per combination of
/// block types on the way. Never empty, and every shape starts with a
/// [`ProbeStep::Key`].
///
/// Segments are split on `.` and on the flattened group separator `__`, so
/// the dotted and the flattened group forms shape alike. The walk stops at the
/// first segment that is not a container field — a value field (a
/// relationship's `.id` is judged by the relationship field's own rule), a row
/// `id`, `_block_type`, a companion column, or an unknown name, which the
/// query itself rejects — keeping that segment as the leaf key.
pub(super) fn probe_shapes(fields: &[FieldDefinition], path: &str) -> Vec<Vec<ProbeStep>> {
    let segments: Vec<&str> = path
        .split('.')
        .flat_map(|segment| segment.split("__"))
        .filter(|segment| !segment.is_empty())
        .collect();

    if segments.is_empty() {
        return vec![vec![ProbeStep::Key(path.to_string())]];
    }

    shapes_at(&flatten_array_sub_fields(fields), &segments)
}

/// The shapes of `segments` (non-empty) starting at a level holding `fields`.
fn shapes_at(fields: &[&FieldDefinition], segments: &[&str]) -> Vec<Vec<ProbeStep>> {
    let Some((&name, rest)) = segments.split_first() else {
        return Vec::new();
    };

    let key = ProbeStep::Key(name.to_string());

    // The path ends here, or goes on past a field this level does not hold.
    let Some(field) = fields
        .iter()
        .find(|f| f.name == name)
        .filter(|_| !rest.is_empty())
    else {
        return vec![vec![key]];
    };

    match field_children(field) {
        FieldChildren::Group(sub) => {
            prefixed(&[key], shapes_at(&flatten_array_sub_fields(sub), rest))
        }
        FieldChildren::Array(sub) => prefixed(
            &[key, ProbeStep::Row(None)],
            shapes_at(&flatten_array_sub_fields(sub), rest),
        ),
        FieldChildren::Blocks(blocks) => block_shapes(&key, blocks, rest),
        FieldChildren::Leaf | FieldChildren::Wrapper(_) | FieldChildren::Tabs(_) => vec![vec![key]],
    }
}

/// The shapes below a blocks field reached by `key`: one per block type
/// holding the next segment's field — or, for a segment no block type names
/// (a row `id`, `_block_type`), one per block type. A blocks field without
/// block types holds no rows, so only the field itself is judged.
fn block_shapes(key: &ProbeStep, blocks: &[BlockDefinition], rest: &[&str]) -> Vec<Vec<ProbeStep>> {
    if blocks.is_empty() {
        return vec![vec![key.clone()]];
    }

    let holds_next = |block: &&BlockDefinition| {
        flatten_array_sub_fields(&block.fields)
            .iter()
            .any(|f| rest.first().is_some_and(|next| f.name == *next))
    };

    let mut candidates: Vec<&BlockDefinition> = blocks.iter().filter(holds_next).collect();
    if candidates.is_empty() {
        candidates = blocks.iter().collect();
    }

    candidates
        .into_iter()
        .flat_map(|block| {
            let head = [key.clone(), ProbeStep::Row(Some(block.block_type.clone()))];

            prefixed(
                &head,
                shapes_at(&flatten_array_sub_fields(&block.fields), rest),
            )
        })
        .collect()
}

/// Every shape in `tails`, each preceded by `head`.
fn prefixed(head: &[ProbeStep], tails: Vec<Vec<ProbeStep>>) -> Vec<Vec<ProbeStep>> {
    tails
        .into_iter()
        .map(|tail| head.iter().cloned().chain(tail).collect())
        .collect()
}

/// A document holding exactly `shape`, with a `null` leaf.
pub(super) fn probe_document(shape: &[ProbeStep]) -> Document {
    let value = shape.iter().rev().fold(Value::Null, wrap);

    let mut doc = Document::new(String::new());

    if let Value::Object(map) = value {
        doc.fields = map.into_iter().collect();
    }

    doc
}

/// Wrap `inner` in one probe step.
fn wrap(inner: Value, step: &ProbeStep) -> Value {
    match step {
        ProbeStep::Key(key) => {
            let mut level = Map::new();
            level.insert(key.clone(), inner);

            Value::Object(level)
        }
        ProbeStep::Row(block_type) => {
            let mut row = match inner {
                Value::Object(row) => row,
                _ => Map::new(),
            };

            if let Some(block_type) = block_type {
                row.insert(
                    BLOCK_TYPE_KEY.to_string(),
                    Value::String(block_type.clone()),
                );
            }

            Value::Array(vec![Value::Object(row)])
        }
    }
}

/// Whether the leaf of `shape` is still in `doc` — every container on the way
/// and the leaf itself survived the strip.
pub(super) fn leaf_survives(doc: &Document, shape: &[ProbeStep]) -> bool {
    let Some((ProbeStep::Key(root), rest)) = shape.split_first() else {
        return false;
    };

    let mut current = doc.fields.get(root);

    for step in rest {
        current = current.and_then(|value| match step {
            ProbeStep::Key(key) => value.as_object().and_then(|level| level.get(key)),
            ProbeStep::Row(_) => value.as_array().and_then(|rows| rows.first()),
        });
    }

    current.is_some()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::{FieldTab, FieldType, RelationshipConfig};

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn container(
        name: &str,
        field_type: FieldType,
        fields: Vec<FieldDefinition>,
    ) -> FieldDefinition {
        FieldDefinition::builder(name, field_type)
            .fields(fields)
            .build()
    }

    fn blocks(name: &str, types: Vec<BlockDefinition>) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Blocks)
            .blocks(types)
            .build()
    }

    fn key(name: &str) -> ProbeStep {
        ProbeStep::Key(name.to_string())
    }

    fn row(block_type: Option<&str>) -> ProbeStep {
        ProbeStep::Row(block_type.map(str::to_string))
    }

    fn probe_json(shape: &[ProbeStep]) -> Value {
        Value::Object(probe_document(shape).fields.into_iter().collect())
    }

    #[test]
    fn a_top_level_field_is_its_own_key() {
        let fields = vec![text("title")];

        assert_eq!(probe_shapes(&fields, "title"), vec![vec![key("title")]]);
    }

    #[test]
    fn dotted_and_flattened_group_paths_shape_alike() {
        let fields = vec![container(
            "seo",
            FieldType::Group,
            vec![container("meta", FieldType::Group, vec![text("secret")])],
        )];

        let expected = vec![vec![key("seo"), key("meta"), key("secret")]];
        assert_eq!(probe_shapes(&fields, "seo__meta__secret"), expected);
        assert_eq!(probe_shapes(&fields, "seo.meta.secret"), expected);

        assert_eq!(
            probe_json(&expected[0]),
            json!({ "seo": { "meta": { "secret": null } } })
        );
    }

    #[test]
    fn an_array_sub_field_sits_in_one_row() {
        let fields = vec![container("items", FieldType::Array, vec![text("secret")])];

        let shapes = probe_shapes(&fields, "items.secret");
        assert_eq!(shapes, vec![vec![key("items"), row(None), key("secret")]]);
        assert_eq!(
            probe_json(&shapes[0]),
            json!({ "items": [{ "secret": null }] })
        );
    }

    #[test]
    fn a_nested_array_nests_its_rows() {
        let inner = container("inner", FieldType::Array, vec![text("secret")]);
        let fields = vec![container("items", FieldType::Array, vec![inner])];

        let shapes = probe_shapes(&fields, "items.inner.secret");
        assert_eq!(
            probe_json(&shapes[0]),
            json!({ "items": [{ "inner": [{ "secret": null }] }] })
        );
    }

    /// A block step fans out only into the block types holding the next field.
    #[test]
    fn a_block_sub_field_shapes_one_row_per_block_type_holding_it() {
        let fields = vec![blocks(
            "content",
            vec![
                BlockDefinition::new("hero", vec![text("body")]),
                BlockDefinition::new("quote", vec![text("author")]),
                BlockDefinition::new("text", vec![text("body")]),
            ],
        )];

        let shapes = probe_shapes(&fields, "content.body");
        assert_eq!(
            shapes,
            vec![
                vec![key("content"), row(Some("hero")), key("body")],
                vec![key("content"), row(Some("text")), key("body")],
            ]
        );
        assert_eq!(
            probe_json(&shapes[0]),
            json!({ "content": [{ "_block_type": "hero", "body": null }] })
        );
    }

    /// A row's own `id` / `_block_type` belongs to every block type.
    #[test]
    fn a_row_id_shapes_every_block_type() {
        let fields = vec![blocks(
            "content",
            vec![
                BlockDefinition::new("hero", vec![text("body")]),
                BlockDefinition::new("quote", vec![text("author")]),
            ],
        )];

        assert_eq!(probe_shapes(&fields, "content.id").len(), 2);
        assert_eq!(probe_shapes(&fields, "content._block_type").len(), 2);
    }

    #[test]
    fn layout_wrappers_are_transparent() {
        let tabs = FieldDefinition::builder("layout", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "Main",
                vec![container("items", FieldType::Array, vec![text("secret")])],
            )])
            .build();

        assert_eq!(
            probe_shapes(&[tabs], "items.secret"),
            vec![vec![key("items"), row(None), key("secret")]]
        );
    }

    /// A relationship's `.id` is judged by the relationship field itself.
    #[test]
    fn a_relationship_path_stops_at_the_relationship() {
        let tags = FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build();

        assert_eq!(probe_shapes(&[tags], "tags.id"), vec![vec![key("tags")]]);
    }

    #[test]
    fn an_unknown_segment_becomes_the_leaf() {
        let fields = vec![container("items", FieldType::Array, vec![text("name")])];

        assert_eq!(
            probe_shapes(&fields, "items.nope.deeper"),
            vec![vec![key("items"), row(None), key("nope")]]
        );
        assert_eq!(
            probe_shapes(&fields, "created_at"),
            vec![vec![key("created_at")]]
        );
    }

    #[test]
    fn the_leaf_survives_only_with_every_container() {
        let shape = vec![key("items"), row(None), key("secret")];
        let mut doc = probe_document(&shape);
        assert!(leaf_survives(&doc, &shape));

        doc.fields.insert("items".to_string(), json!([{}]));
        assert!(!leaf_survives(&doc, &shape), "stripped leaf");

        doc.fields.remove("items");
        assert!(!leaf_survives(&doc, &shape), "stripped container");
    }
}
