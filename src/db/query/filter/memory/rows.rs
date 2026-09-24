//! Filters on the rows of an array or blocks field for the in-memory
//! evaluator: some row — at any nesting the path descends through — whose value
//! at the rest of the path satisfies the filter, the reading the SQL builder's
//! `EXISTS` subquery applies (see `filter::subquery`). A list in the row is
//! read element by element (see `super::lists`); a document with no rows
//! matches no filter on them. A top-level row's own `id` is filterable; a row
//! nested in another row's JSON is not addressed by id, and `_block_type` is
//! read only in a block row — both as the SQL path resolves them.

use serde_json::Value;

use super::{
    lists::{list_elements, matches_list, reference_ids},
    matches_null, matches_value,
};
use crate::{
    core::{
        BLOCK_TYPE_KEY, BlockDefinition, DocumentFields, FieldChildren, FieldDefinition, FieldType,
        field_children, find_field, flatten_array_sub_fields,
    },
    db::{
        Filter, FilterOp,
        query::{
            filter::{elements::ListLeaf, resolve::ROW_ID},
            helpers::ListPlace,
        },
    },
};

/// Evaluate a filter on the rows of an array or blocks field — `None` when the
/// filter's path doesn't start at one.
pub(super) fn matches_row_path(
    data: &DocumentFields,
    filter: &Filter,
    fields: &[FieldDefinition],
) -> Option<bool> {
    let (root, rest) = filter.field.split_once('.')?;
    let root_def = find_field(root, fields)?;

    let level = match field_children(root_def) {
        FieldChildren::Array(sub) => RowLevel::new(flatten_array_sub_fields(sub), false),
        FieldChildren::Blocks(defs) => RowLevel::new(block_fields(defs), true),
        _ => return None,
    };

    if rest == ROW_ID {
        return Some(matches_row_id(data.get(root), &filter.op));
    }

    let segments: Vec<&str> = rest.split('.').collect();
    let path = RowPath::new(&segments, &filter.op);

    Some(path.matches_rows(data.get(root), &level))
}

/// Whether some top-level row's own id satisfies `op`.
fn matches_row_id(rows: Option<&Value>, op: &FilterOp) -> bool {
    let Some(Value::Array(rows)) = rows else {
        return false;
    };

    let leaf = RowLeaf::Value(Some(FieldType::Text));

    rows.iter().any(|row| leaf.matches(row.get(ROW_ID), op))
}

/// The fields a row holds, and whether it is a block row — the only kind that
/// carries a `_block_type`.
struct RowLevel<'a> {
    fields: Vec<&'a FieldDefinition>,
    block_row: bool,
}

impl<'a> RowLevel<'a> {
    fn new(fields: Vec<&'a FieldDefinition>, block_row: bool) -> Self {
        Self { fields, block_row }
    }
}

/// Every block type's fields, layout wrappers flattened: a block row holds
/// them all under its own keys.
fn block_fields(defs: &[BlockDefinition]) -> Vec<&FieldDefinition> {
    defs.iter()
        .flat_map(|def| flatten_array_sub_fields(&def.fields))
        .collect()
}

/// The rest of a filter path below its array or blocks field, and the
/// operator the value it reaches is tested with.
struct RowPath<'a> {
    segments: &'a [&'a str],
    op: &'a FilterOp,
}

impl<'a> RowPath<'a> {
    fn new(segments: &'a [&'a str], op: &'a FilterOp) -> Self {
        Self { segments, op }
    }

    /// The path below the first segment.
    fn descend(&self) -> Self {
        Self::new(self.segments.get(1..).unwrap_or_default(), self.op)
    }

    /// Whether some row of `rows` — a missing value holds none — satisfies the
    /// path.
    fn matches_rows(&self, rows: Option<&Value>, level: &RowLevel<'_>) -> bool {
        let Some(Value::Array(rows)) = rows else {
            return false;
        };

        rows.iter().any(|row| self.matches_at(Some(row), level))
    }

    /// Whether the value the path reaches from `object` — an object of the
    /// `level`'s fields, or a missing one, whose values are then missing too —
    /// satisfies the operator.
    fn matches_at(&self, object: Option<&Value>, level: &RowLevel<'_>) -> bool {
        let Some((segment, remaining)) = self.segments.split_first() else {
            return false;
        };
        let value = object.and_then(|object| object.get(*segment));

        if *segment == BLOCK_TYPE_KEY {
            let leaf = RowLeaf::Value(Some(FieldType::Text));

            return level.block_row && remaining.is_empty() && leaf.matches(value, self.op);
        }

        let Some(field) = level.fields.iter().find(|f| f.name == *segment) else {
            return false;
        };

        match field_children(field) {
            FieldChildren::Array(sub) => {
                let rows = RowLevel::new(flatten_array_sub_fields(sub), false);

                self.descend().matches_rows(value, &rows)
            }
            FieldChildren::Blocks(defs) => {
                let rows = RowLevel::new(block_fields(defs), true);

                self.descend().matches_rows(value, &rows)
            }
            FieldChildren::Group(sub) => {
                let group = RowLevel::new(flatten_array_sub_fields(sub), false);

                self.descend().matches_at(value, &group)
            }
            FieldChildren::Wrapper(_) | FieldChildren::Tabs(_) => false,
            FieldChildren::Leaf => {
                remaining.is_empty() && RowLeaf::of(field).matches(value, self.op)
            }
        }
    }
}

/// What a leaf inside a row holds.
enum RowLeaf {
    /// A single value, of this type when known.
    Value(Option<FieldType>),
    /// A list, read element by element.
    List(ListLeaf),
}

impl RowLeaf {
    fn of(field: &FieldDefinition) -> Self {
        ListLeaf::of(field).map_or_else(|| Self::Value(Some(field.field_type.clone())), Self::List)
    }

    /// Whether `value` — missing or null alike, as SQL reads a key a row's
    /// JSON lacks as NULL — satisfies `op`.
    fn matches(&self, value: Option<&Value>, op: &FilterOp) -> bool {
        match self {
            Self::List(ListLeaf::Scalar(field_type)) => {
                let elements = list_elements(value, field_type, ListPlace::Row);

                matches_list(&elements, op, Some(field_type))
            }
            Self::List(ListLeaf::References { polymorphic }) => {
                let ids = reference_ids(value, *polymorphic);

                matches_list(&ids, op, Some(&FieldType::Text))
            }
            Self::Value(field_type) => match value.filter(|v| !v.is_null()) {
                Some(value) => matches_value(value, op, field_type.as_ref()),
                None => matches_null(op),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::db::{InMemoryConn, query::filter::row_paths_fixture::assert_row_paths_agree};

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn fields() -> Vec<FieldDefinition> {
        let dims = FieldDefinition::builder("dims", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("width", FieldType::Number).build(),
            ])
            .build();
        let row = FieldDefinition::builder("layout", FieldType::Row)
            .fields(vec![text("caption")])
            .build();
        let quotes = FieldDefinition::builder("quotes", FieldType::Array)
            .fields(vec![text("who")])
            .build();

        vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![text("name"), dims])
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![
                    BlockDefinition::new("image", vec![row]),
                    BlockDefinition::new("talk", vec![quotes]),
                ])
                .build(),
        ]
    }

    fn document() -> DocumentFields {
        DocumentFields::from(HashMap::from([
            (
                "items".to_string(),
                json!([
                    { "name": "a", "dims": { "width": 10 } },
                    { "name": "b" },
                ]),
            ),
            (
                "content".to_string(),
                json!([
                    { "_block_type": "image", "caption": "Sunset" },
                    { "_block_type": "talk", "quotes": [{ "who": "Ada" }] },
                ]),
            ),
        ]))
    }

    fn matches(field: &str, op: FilterOp) -> Option<bool> {
        let filter = Filter {
            field: field.to_string(),
            op,
        };

        matches_row_path(&document(), &filter, &fields())
    }

    /// Some row satisfies the filter — for a negative operator too, the way
    /// the SQL `EXISTS` over the rows reads it.
    #[test]
    fn some_row_satisfies_the_filter() {
        assert_eq!(
            matches("items.name", FilterOp::Equals("b".into())),
            Some(true)
        );
        assert_eq!(
            matches("items.name", FilterOp::Equals("z".into())),
            Some(false)
        );
        assert_eq!(
            matches("items.name", FilterOp::NotEquals("a".into())),
            Some(true)
        );
        assert_eq!(
            matches("items.dims.width", FilterOp::GreaterThan("9".into())),
            Some(true)
        );
        assert_eq!(matches("items.dims.width", FilterOp::NotExists), Some(true));
    }

    /// Blocks rows are read with every block type's fields, a layout row's
    /// field by its own name, nested rows at any depth, and the block type.
    #[test]
    fn blocks_rows_are_read_at_any_depth() {
        assert_eq!(
            matches("content.caption", FilterOp::Equals("Sunset".into())),
            Some(true)
        );
        assert_eq!(
            matches("content.quotes.who", FilterOp::Equals("Ada".into())),
            Some(true)
        );
        assert_eq!(
            matches("content._block_type", FilterOp::Equals("talk".into())),
            Some(true)
        );
        assert_eq!(
            matches("content.quotes.who", FilterOp::Equals("Bob".into())),
            Some(false)
        );
    }

    /// A row path is read through the rows, never as an absent top-level
    /// value — which would let `not_exists` match a document whose every row
    /// holds the value, one SQL doesn't match. No rows match nothing.
    #[test]
    fn a_row_path_is_not_read_as_a_missing_field() {
        assert_eq!(matches("items.name", FilterOp::NotExists), Some(false));

        let empty = DocumentFields::from(HashMap::from([("items".to_string(), json!([]))]));
        let filter = Filter {
            field: "items.name".to_string(),
            op: FilterOp::NotExists,
        };
        assert_eq!(matches_row_path(&empty, &filter, &fields()), Some(false));
    }

    /// A path not starting at an array or blocks field is left to the caller.
    #[test]
    fn other_paths_are_not_row_paths() {
        assert_eq!(matches("title", FilterOp::Exists), None);
        assert_eq!(matches("unknown.x", FilterOp::Exists), None);
    }

    /// A top-level row's own id is matched — an array row's and a block row's
    /// alike; a nested row is not addressed by id.
    #[test]
    fn a_top_level_row_id_is_matched() {
        let doc = DocumentFields::from(HashMap::from([
            ("items".to_string(), json!([{ "id": "r1", "name": "a" }])),
            (
                "content".to_string(),
                json!([{ "id": "b1", "_block_type": "talk", "quotes": [{ "who": "Ada" }] }]),
            ),
        ]));
        let at = |field: &str, op: FilterOp| {
            let filter = Filter {
                field: field.to_string(),
                op,
            };

            matches_row_path(&doc, &filter, &fields())
        };

        assert_eq!(at("items.id", FilterOp::Equals("r1".into())), Some(true));
        assert_eq!(at("items.id", FilterOp::Equals("b1".into())), Some(false));
        assert_eq!(at("content.id", FilterOp::Equals("b1".into())), Some(true));
        assert_eq!(at("content.quotes.id", FilterOp::NotExists), Some(false));
    }

    /// `_block_type` is a block row's; an array row or a group has none, so a
    /// path asking for it there matches nothing (SQL refuses it).
    #[test]
    fn block_type_is_read_only_in_a_block_row() {
        assert_eq!(
            matches("items._block_type", FilterOp::NotExists),
            Some(false)
        );
        assert_eq!(
            matches("items.dims._block_type", FilterOp::NotExists),
            Some(false)
        );
    }

    /// SQL and the in-memory evaluator read every row path alike (the shared
    /// fixture runs the same check on Postgres).
    #[test]
    fn row_paths_agree_between_sql_and_memory() {
        assert_row_paths_agree(&InMemoryConn::open(), "posts");
    }
}
