//! The fields a filter path can name inside a row — shared by the SQL row
//! walker (`resolve::json_walk`) and the in-memory evaluator (`memory::rows`),
//! so both resolve a name the same way.
//!
//! A group or array row holds one set of fields. A block row holds the fields
//! of its own block type, and block types are independent: two types may give
//! the same name to fields of different kinds (a number in one, text in
//! another; a has-many list in one, a single value in another; a scalar in one,
//! a group in another). A name every declaring block type defines alike reads
//! the same in every row; a name the types define differently is read per
//! block type — each row with its own type's definition.

use crate::core::{
    BlockDefinition, FieldChildren, FieldDefinition, field_children, flatten_array_sub_fields,
};

use super::elements::ListLeaf;

/// A field a row holds, with the block type declaring it when the row is a
/// block row.
#[derive(Debug, Clone, Copy)]
pub(in crate::db::query::filter) struct RowField<'a> {
    block_type: Option<&'a str>,
    field: &'a FieldDefinition,
}

impl<'a> RowField<'a> {
    fn new(block_type: Option<&'a str>, field: &'a FieldDefinition) -> Self {
        Self { block_type, field }
    }
}

/// The fields of a group's object or an array's row: layout wrappers
/// flattened, no block type.
pub(in crate::db::query::filter) fn plain_row_fields(
    fields: &[FieldDefinition],
) -> Vec<RowField<'_>> {
    flatten_array_sub_fields(fields)
        .into_iter()
        .map(|field| RowField::new(None, field))
        .collect()
}

/// The fields of a blocks field's rows: every block type's fields, layout
/// wrappers flattened, each with the block type declaring it.
pub(in crate::db::query::filter) fn block_row_fields(
    blocks: &[BlockDefinition],
) -> Vec<RowField<'_>> {
    blocks
        .iter()
        .flat_map(|block| {
            flatten_array_sub_fields(&block.fields)
                .into_iter()
                .map(|field| RowField::new(Some(block.block_type.as_str()), field))
        })
        .collect()
}

/// How a row holding `fields` reads the name `name`.
pub(in crate::db::query::filter) enum RowLookup<'a> {
    /// No field of that name.
    Unknown,
    /// One definition, whatever the row's block type.
    Uniform(&'a FieldDefinition),
    /// Block types define the name differently: each candidate is read only in
    /// rows of its own block type.
    PerBlockType(Vec<(&'a str, &'a FieldDefinition)>),
}

/// Look `name` up among a row's `fields`.
pub(in crate::db::query::filter) fn lookup_row_field<'a>(
    fields: &[RowField<'a>],
    name: &str,
) -> RowLookup<'a> {
    let candidates: Vec<RowField<'a>> = fields
        .iter()
        .filter(|candidate| candidate.field.name == name)
        .copied()
        .collect();

    let Some(first) = candidates.first() else {
        return RowLookup::Unknown;
    };

    if candidates
        .iter()
        .all(|candidate| same_filter_shape(first.field, candidate.field))
    {
        return RowLookup::Uniform(first.field);
    }

    let per_type = candidates
        .iter()
        .filter_map(|candidate| Some((candidate.block_type?, candidate.field)))
        .collect();

    RowLookup::PerBlockType(per_type)
}

/// Whether a filter reads `a` and `b` alike: the same type, the same list
/// reading, and — for a container — the same fields below, by name, at any
/// depth.
fn same_filter_shape(a: &FieldDefinition, b: &FieldDefinition) -> bool {
    if a.field_type != b.field_type || ListLeaf::of(a) != ListLeaf::of(b) {
        return false;
    }

    match (field_children(a), field_children(b)) {
        (FieldChildren::Group(x), FieldChildren::Group(y))
        | (FieldChildren::Array(x), FieldChildren::Array(y)) => same_fields(x, y),
        (FieldChildren::Blocks(x), FieldChildren::Blocks(y)) => same_blocks(x, y),
        (FieldChildren::Leaf, FieldChildren::Leaf) => true,
        _ => false,
    }
}

/// Whether two field lists hold the same names, each read alike.
fn same_fields(a: &[FieldDefinition], b: &[FieldDefinition]) -> bool {
    let a = flatten_array_sub_fields(a);
    let b = flatten_array_sub_fields(b);

    a.len() == b.len()
        && a.iter().all(|x| {
            b.iter()
                .any(|y| x.name == y.name && same_filter_shape(x, y))
        })
}

/// Whether two blocks fields hold the same block types, each with the same
/// fields.
fn same_blocks(a: &[BlockDefinition], b: &[BlockDefinition]) -> bool {
    a.len() == b.len()
        && a.iter().all(|x| {
            b.iter()
                .any(|y| x.block_type == y.block_type && same_fields(&x.fields, &y.fields))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::FieldType;

    fn field(name: &str, field_type: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, field_type).build()
    }

    fn group(name: &str, fields: Vec<FieldDefinition>) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Group)
            .fields(fields)
            .build()
    }

    fn uniform_name(lookup: &RowLookup<'_>) -> Option<String> {
        match lookup {
            RowLookup::Uniform(field) => Some(field.name.clone()),
            _ => None,
        }
    }

    fn per_type(lookup: RowLookup<'_>) -> Vec<(String, FieldType)> {
        match lookup {
            RowLookup::PerBlockType(candidates) => candidates
                .into_iter()
                .map(|(block_type, f)| (block_type.to_string(), f.field_type.clone()))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// A name every declaring block type defines alike reads uniformly; one
    /// they define differently is read per block type.
    #[test]
    fn block_types_defining_a_name_differently_are_read_per_type() {
        let blocks = vec![
            BlockDefinition::new(
                "stat",
                vec![
                    field("score", FieldType::Number),
                    field("title", FieldType::Text),
                ],
            ),
            BlockDefinition::new(
                "note",
                vec![
                    field("score", FieldType::Text),
                    field("title", FieldType::Text),
                ],
            ),
        ];
        let fields = block_row_fields(&blocks);

        assert_eq!(
            uniform_name(&lookup_row_field(&fields, "title")),
            Some("title".to_string())
        );
        assert_eq!(
            per_type(lookup_row_field(&fields, "score")),
            vec![
                ("stat".to_string(), FieldType::Number),
                ("note".to_string(), FieldType::Text)
            ]
        );
        assert!(matches!(
            lookup_row_field(&fields, "nope"),
            RowLookup::Unknown
        ));
    }

    /// A has-many list and a single value of the same type differ; so do a
    /// scalar and a group, and two groups holding different fields.
    #[test]
    fn list_and_container_shapes_are_compared() {
        let tags = FieldDefinition::builder("tags", FieldType::Text)
            .has_many(true)
            .build();

        assert!(!same_filter_shape(&tags, &field("tags", FieldType::Text)));
        assert!(!same_filter_shape(
            &field("info", FieldType::Text),
            &group("info", vec![field("x", FieldType::Text)])
        ));
        assert!(!same_filter_shape(
            &group("info", vec![field("x", FieldType::Text)]),
            &group("info", vec![field("x", FieldType::Number)])
        ));
        assert!(same_filter_shape(
            &group("info", vec![field("x", FieldType::Text)]),
            &group("info", vec![field("x", FieldType::Text)])
        ));
    }
}
