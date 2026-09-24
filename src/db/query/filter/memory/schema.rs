//! What the in-memory evaluator knows of the constrained collection: each
//! filter path's leaf, and the fields a row path descends into.

use std::collections::HashMap;

use crate::{
    core::{FieldDefinition, FieldType, prefixed_name, walk_leaf_fields},
    db::query::{filter::resolve::typed_system_columns, helpers::is_polymorphic},
};

/// What the matcher knows of a constrained field path.
pub(super) enum Leaf {
    /// A single value of this type.
    Value(FieldType),
    /// A scalar has-many list of this element type.
    List(FieldType),
    /// The `.id` of a has-many relationship/upload stored under `root`, whose
    /// ids the SQL path reads from the junction rows.
    References { root: String, polymorphic: bool },
}

impl Leaf {
    /// The type an operand is bound as: the value's, a list element's, or a
    /// referenced id's (text).
    pub(super) fn operand_type(&self) -> FieldType {
        match self {
            Self::Value(field_type) | Self::List(field_type) => field_type.clone(),
            Self::References { .. } => FieldType::Text,
        }
    }
}

/// What the matcher knows of the constrained collection: its leaves by filter
/// path, and its fields, whose array and blocks rows a path may descend into.
pub(super) struct Schema<'a> {
    pub(super) types: HashMap<String, Leaf>,
    pub(super) fields: &'a [FieldDefinition],
}

impl<'a> Schema<'a> {
    pub(super) fn new(fields: &'a [FieldDefinition]) -> Self {
        Self {
            types: field_type_map(fields),
            fields,
        }
    }
}

/// Build a filter-path → leaf map for the field tree: every leaf by its flat
/// column name (`meta__color`), and every has-many relationship or upload —
/// top-level or inside groups — by the `rel.id` path the SQL filter accepts
/// for it, keyed by its flat name (`seo__tags.id`).
fn field_type_map(fields: &[FieldDefinition]) -> HashMap<String, Leaf> {
    // The system timestamps no field defines compare as SQL types them.
    let mut types: HashMap<String, Leaf> = typed_system_columns()
        .map(|(col, field_type)| (col.to_string(), Leaf::Value(field_type)))
        .collect();

    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, _| {
        let name = prefixed_name(prefix, &field.name);

        if field.is_has_many_reference() {
            let leaf = Leaf::References {
                root: name.clone(),
                polymorphic: is_polymorphic(field),
            };

            types.insert(format!("{name}.id"), leaf);
        }

        types.insert(name, leaf_of(field));
        Ok(())
    });

    types
}

fn leaf_of(field: &FieldDefinition) -> Leaf {
    if field.is_has_many_scalar() {
        return Leaf::List(field.field_type.clone());
    }

    Leaf::Value(field.field_type.clone())
}
