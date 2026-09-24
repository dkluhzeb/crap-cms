//! Filters on the rows of an array or blocks field for the in-memory
//! evaluator: some row — at any nesting the path descends through — whose value
//! at the rest of the path satisfies the filter, the reading the SQL builder's
//! `EXISTS` subquery applies (see `filter::subquery`). A list in the row is
//! read element by element (see `super::lists`); a document with no rows
//! matches no filter on them. A top-level row's own `id` is filterable; a row
//! nested in another row's JSON is not addressed by id, and `_block_type` is
//! read only in a block row — both as the SQL path resolves them. A name block
//! types define differently is read in each block row with its own type's
//! definition (see `filter::row_fields`) — a row of a type declaring no field
//! of that name reads it as absent, as it would under one shared definition —
//! and a field storing no value (a join) matches nothing. The array or blocks
//! field may sit inside groups, its rows then under its flat key
//! (`seo__items`).

use serde_json::Value;

use super::{
    lists::{list_elements, matches_list, reference_ids},
    matches_null, matches_value,
};
use crate::{
    core::{
        BLOCK_TYPE_KEY, DocumentFields, FieldChildren, FieldDefinition, FieldType, field_children,
    },
    db::{
        Filter, FilterOp,
        query::{
            filter::{
                elements::ListLeaf,
                operators::operand_fits,
                resolve::{ROW_ID, container_root},
                row_fields::{
                    RowField, RowLookup, block_row_fields, lookup_row_field, plain_row_fields,
                },
            },
            helpers::ListPlace,
        },
    },
};

/// Evaluate a filter on the rows of an array or blocks field — `None` when the
/// filter's path doesn't start at one. `data` is group-flattened.
pub(super) fn matches_row_path(
    data: &DocumentFields,
    filter: &Filter,
    fields: &[FieldDefinition],
) -> Option<bool> {
    let container = container_root(&filter.field, fields)?;

    let level = match field_children(container.field) {
        FieldChildren::Array(sub) => RowLevel::new(plain_row_fields(sub), false),
        FieldChildren::Blocks(defs) => RowLevel::new(block_row_fields(defs), true),
        _ => return None,
    };

    let rows = data.get(container.name.as_str());

    if container.rest == ROW_ID {
        return Some(matches_row_id(rows, &filter.op));
    }

    let segments: Vec<&str> = container.rest.split('.').collect();
    let path = RowPath::new(&segments, &filter.op);

    Some(path.matches_rows(rows, &level))
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
    fields: Vec<RowField<'a>>,
    block_row: bool,
}

impl<'a> RowLevel<'a> {
    fn new(fields: Vec<RowField<'a>>, block_row: bool) -> Self {
        Self { fields, block_row }
    }

    /// How the row `object` reads `name`: with the definition every block type
    /// shares, or the row's own block type's — absent when block types define
    /// the name differently and the row's type declares none of them.
    fn reading(&self, object: Option<&Value>, name: &str) -> RowReading<'a> {
        let candidates = match lookup_row_field(&self.fields, name) {
            RowLookup::Unknown => return RowReading::Unknown,
            RowLookup::Uniform(field) => return RowReading::Field(field),
            RowLookup::PerBlockType(candidates) => candidates,
        };

        let row_type = object
            .and_then(|object| object.get(BLOCK_TYPE_KEY))
            .and_then(Value::as_str);

        candidates
            .into_iter()
            .find(|(block_type, _)| Some(*block_type) == row_type)
            .map_or(RowReading::Absent, |(_, field)| RowReading::Field(field))
    }
}

/// How a row reads one name of a filter path.
enum RowReading<'a> {
    /// No block type declares the name.
    Unknown,
    /// Read with this definition.
    Field(&'a FieldDefinition),
    /// The row's block type declares no field of the name, which other block
    /// types define differently: the value is absent.
    Absent,
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

        let field = match level.reading(object, segment) {
            RowReading::Unknown => return false,
            RowReading::Field(field) => field,
            RowReading::Absent => return self.matches_absent(level),
        };

        match field_children(field) {
            FieldChildren::Array(sub) => {
                let rows = RowLevel::new(plain_row_fields(sub), false);

                self.descend().matches_rows(value, &rows)
            }
            FieldChildren::Blocks(defs) => {
                let rows = RowLevel::new(block_row_fields(defs), true);

                self.descend().matches_rows(value, &rows)
            }
            FieldChildren::Group(sub) => {
                let group = RowLevel::new(plain_row_fields(sub), false);

                self.descend().matches_at(value, &group)
            }
            FieldChildren::Wrapper(_) | FieldChildren::Tabs(_) => false,
            FieldChildren::Leaf => {
                remaining.is_empty()
                    && field.field_type.is_writable()
                    && RowLeaf::of(field).matches(value, self.op)
            }
        }
    }

    /// Whether an absent value — the NULL SQL reads in a row whose block type
    /// declares none of the path's definitions — satisfies the operator. Only
    /// when SQL builds the filter at all: some declaring block type's reading
    /// must hold the path and take the operand, as the absent reading alone
    /// never validates one.
    fn matches_absent(&self, level: &RowLevel<'_>) -> bool {
        readable(&level.fields, level.block_row, self.segments, self.op)
            && RowLeaf::Value(None).matches(None, self.op)
    }
}

/// Whether SQL has a reading of `segments` below a row holding `fields` that
/// takes the operator's operand — the path reaches a value field of some
/// declaring definition, at every fork.
fn readable(fields: &[RowField<'_>], block_row: bool, segments: &[&str], op: &FilterOp) -> bool {
    let Some((segment, rest)) = segments.split_first() else {
        return false;
    };

    if *segment == BLOCK_TYPE_KEY {
        return block_row && rest.is_empty() && operand_fits(Some(&FieldType::Text), op);
    }

    match lookup_row_field(fields, segment) {
        RowLookup::Unknown => false,
        RowLookup::Uniform(field) => field_readable(field, rest, op),
        RowLookup::PerBlockType(candidates) => candidates
            .into_iter()
            .any(|(_, field)| field_readable(field, rest, op)),
    }
}

/// Whether the path `rest` below `field` has a reading taking the operand.
fn field_readable(field: &FieldDefinition, rest: &[&str], op: &FilterOp) -> bool {
    match field_children(field) {
        FieldChildren::Array(sub) | FieldChildren::Group(sub) => {
            readable(&plain_row_fields(sub), false, rest, op)
        }
        FieldChildren::Blocks(defs) => readable(&block_row_fields(defs), true, rest, op),
        FieldChildren::Wrapper(_) | FieldChildren::Tabs(_) => false,
        FieldChildren::Leaf => {
            rest.is_empty()
                && field.field_type.is_writable()
                && operand_fits(RowLeaf::of(field).operand_type().as_ref(), op)
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

    /// The type an operand is bound as: the value's, or a list element's.
    fn operand_type(&self) -> Option<FieldType> {
        match self {
            Self::Value(field_type) => field_type.clone(),
            Self::List(list) => Some(list.element_type()),
        }
    }

    /// Whether `value` — missing or null alike, as SQL reads a key a row's
    /// JSON lacks as NULL — satisfies `op`. An operand SQL refuses for the
    /// field matches nothing, as SQL drops that reading.
    fn matches(&self, value: Option<&Value>, op: &FilterOp) -> bool {
        if !operand_fits(self.operand_type().as_ref(), op) {
            return false;
        }

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
    use crate::{
        core::{BlockDefinition, RelationshipConfig},
        db::{InMemoryConn, query::filter::row_paths_fixture::assert_row_paths_agree},
    };

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

    fn at(
        doc: &DocumentFields,
        fields: &[FieldDefinition],
        field: &str,
        op: FilterOp,
    ) -> Option<bool> {
        let filter = Filter {
            field: field.to_string(),
            op,
        };

        matches_row_path(doc, &filter, fields)
    }

    /// Regression: the first block type declaring a name was used for every
    /// row. Each block row is read with its own type's definition.
    #[test]
    fn a_name_typed_differently_per_block_type_reads_each_row_by_its_type() {
        let info = FieldDefinition::builder("info", FieldType::Group)
            .fields(vec![text("x")])
            .build();
        let fields = vec![
            FieldDefinition::builder("cards", FieldType::Blocks)
                .blocks(vec![
                    BlockDefinition::new(
                        "stat",
                        vec![
                            FieldDefinition::builder("score", FieldType::Number).build(),
                            text("info"),
                        ],
                    ),
                    BlockDefinition::new("note", vec![text("score"), info]),
                ])
                .build(),
        ];
        let doc =
            |row: Value| DocumentFields::from(HashMap::from([("cards".to_string(), json!([row]))]));

        let stat = doc(json!({ "_block_type": "stat", "score": 10, "info": "hi" }));
        let note = doc(json!({ "_block_type": "note", "score": "10", "info": { "x": "deep" } }));

        assert_eq!(
            at(
                &stat,
                &fields,
                "cards.score",
                FilterOp::GreaterThan("9".into())
            ),
            Some(true)
        );
        assert_eq!(
            at(
                &note,
                &fields,
                "cards.info.x",
                FilterOp::Equals("deep".into())
            ),
            Some(true)
        );
        assert_eq!(
            at(&stat, &fields, "cards.info.x", FilterOp::Exists),
            Some(false)
        );
        assert_eq!(
            at(&note, &fields, "cards.info", FilterOp::Exists),
            Some(false)
        );
        assert_eq!(
            at(&stat, &fields, "cards.info", FilterOp::Exists),
            Some(true)
        );
    }

    /// Regression: a row of a block type declaring no field of a name the
    /// declaring types define differently matched nothing — while under one
    /// shared definition it reads NULL. It reads as absent in both, at the top
    /// level and in nested block rows; an operand no declaring type takes
    /// still matches nothing.
    #[test]
    fn a_row_of_an_undeclaring_block_type_reads_the_value_as_absent() {
        let per_type = vec![
            BlockDefinition::new(
                "stat",
                vec![FieldDefinition::builder("score", FieldType::Number).build()],
            ),
            BlockDefinition::new("note", vec![text("score")]),
            BlockDefinition::new("blank", vec![]),
        ];
        let fields = vec![
            FieldDefinition::builder("cards", FieldType::Blocks)
                .blocks(per_type.clone())
                .build(),
            FieldDefinition::builder("wraps", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "wrap",
                    vec![
                        FieldDefinition::builder("inner", FieldType::Blocks)
                            .blocks(per_type)
                            .build(),
                    ],
                )])
                .build(),
        ];
        let doc = DocumentFields::from(HashMap::from([
            ("cards".to_string(), json!([{ "_block_type": "blank" }])),
            (
                "wraps".to_string(),
                json!([{ "_block_type": "wrap", "inner": [{ "_block_type": "blank" }] }]),
            ),
        ]));

        for path in ["cards.score", "wraps.inner.score"] {
            assert_eq!(at(&doc, &fields, path, FilterOp::NotExists), Some(true));
            assert_eq!(at(&doc, &fields, path, FilterOp::Exists), Some(false));
            assert_eq!(at(&doc, &fields, path, FilterOp::NotIn(vec![])), Some(true));
            assert_eq!(
                at(&doc, &fields, path, FilterOp::Equals("high".into())),
                Some(false)
            );
        }

        // `x` is declared by no type at all, so the path has no reading.
        assert_eq!(
            at(&doc, &fields, "cards.x", FilterOp::NotExists),
            Some(false)
        );
    }

    /// Regression: an array inside a group could not be filtered. Its rows
    /// are read from the group-flattened document, the group part spelled
    /// either way; a has-many relationship inside a group is left to the
    /// caller, which reads its ids.
    #[test]
    fn an_array_inside_a_group_is_a_row_path() {
        let seo = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("links", FieldType::Array)
                    .fields(vec![text("url")])
                    .build(),
                FieldDefinition::builder("tags", FieldType::Relationship)
                    .relationship(RelationshipConfig::new("tags", true))
                    .build(),
            ])
            .build();
        let fields = vec![seo];
        let doc = DocumentFields::from(HashMap::from([(
            "seo__links".to_string(),
            json!([{ "id": "l1", "url": "a" }]),
        )]));

        assert_eq!(
            at(&doc, &fields, "seo.links.url", FilterOp::Equals("a".into())),
            Some(true)
        );
        assert_eq!(
            at(
                &doc,
                &fields,
                "seo__links.url",
                FilterOp::Equals("b".into())
            ),
            Some(false)
        );
        assert_eq!(
            at(&doc, &fields, "seo.links.id", FilterOp::Equals("l1".into())),
            Some(true)
        );
        assert_eq!(at(&doc, &fields, "seo.tags.id", FilterOp::Exists), None);
    }

    /// Regression: a join field inside a row was read as a value; it stores
    /// none, so it matches nothing — not even `not_exists`.
    #[test]
    fn a_join_field_inside_a_row_matches_nothing() {
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("posts", FieldType::Join).build(),
                ])
                .build(),
        ];
        let doc = DocumentFields::from(HashMap::from([("items".to_string(), json!([{}]))]));

        assert_eq!(
            at(&doc, &fields, "items.posts", FilterOp::NotExists),
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
