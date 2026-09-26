//! Whether a write touches any stored reference.

use crate::{
    core::{
        DocumentFields, FieldChildren, FieldDefinition, FieldType, any_field, field_children,
        flatten_group_fields,
    },
    db::query::helpers::prefixed_name,
};

/// Check whether write data contains any relationship or upload field values.
///
/// When an update doesn't touch any ref-bearing fields, the entire `ref_count`
/// dance (snapshot before, read after, apply deltas) can be skipped — saving
/// 10+ queries on the hot path.
#[must_use]
pub fn data_touches_refs(fields: &[FieldDefinition], data: &DocumentFields, prefix: &str) -> bool {
    // DB-layer edge: the walk reads flat `group__sub` columns, so flatten the
    // canonical nested write data first (idempotent).
    let data = flatten_group_fields(data, fields);

    data_touches_refs_inner(fields, &data, prefix)
}

fn data_touches_refs_inner(
    fields: &[FieldDefinition],
    data: &DocumentFields,
    prefix: &str,
) -> bool {
    for field in fields {
        match field_children(field) {
            FieldChildren::Group(sub) => {
                let new_prefix = prefixed_name(prefix, &field.name);
                if data_touches_refs_inner(sub, data, &new_prefix) {
                    return true;
                }
            }

            FieldChildren::Wrapper(sub) => {
                if data_touches_refs_inner(sub, data, prefix) {
                    return true;
                }
            }

            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    if data_touches_refs_inner(&tab.fields, data, prefix) {
                        return true;
                    }
                }
            }

            FieldChildren::Array(sub) => {
                let col = prefixed_name(prefix, &field.name);

                // Recurse the sub-field tree (groups/arrays/blocks at any
                // depth) so a relationship nested inside a group within the
                // array still arms ref-count recomputation.
                if data.contains_key(&col) && fields_contain_relationship(sub) {
                    return true;
                }
            }

            FieldChildren::Blocks(blocks) => {
                let col = prefixed_name(prefix, &field.name);

                if data.contains_key(&col)
                    && blocks
                        .iter()
                        .any(|b| fields_contain_relationship(&b.fields))
                {
                    return true;
                }
            }

            // Relationship/Upload leaves carry a stored reference — a write to
            // one arms ref-count recomputation. Scalars and the virtual Join
            // never do.
            FieldChildren::Leaf => {
                if matches!(
                    field.field_type,
                    FieldType::Relationship | FieldType::Upload
                ) {
                    let col = prefixed_name(prefix, &field.name);

                    if data.contains_key(&col) {
                        return true;
                    }
                }
            }
        }
    }

    false
}

/// Recursively report whether any field in the subtree references another
/// collection — via the shared `any_field` container walk (descends
/// Group/Array/Blocks/Row/Collapsible/Tabs) and the `is_reference` classifier.
fn fields_contain_relationship(fields: &[FieldDefinition]) -> bool {
    any_field(fields, &|f| f.field_type.is_reference())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::RelationshipConfig;

    fn image() -> FieldDefinition {
        FieldDefinition::builder("image", FieldType::Upload)
            .relationship(RelationshipConfig::new("media", false))
            .build()
    }

    fn data(key: &str) -> DocumentFields {
        let mut data = DocumentFields::new();
        data.insert(key.to_string(), json!("x"));

        data
    }

    /// A write to a reference leaf arms the recount; a write to a scalar
    /// beside it does not.
    #[test]
    fn only_a_written_reference_touches_refs() {
        let fields = vec![
            image(),
            FieldDefinition::builder("title", FieldType::Text).build(),
        ];

        assert!(data_touches_refs(&fields, &data("image"), ""));
        assert!(!data_touches_refs(&fields, &data("title"), ""));
    }

    /// A reference inside a group, or inside an array's rows, is found
    /// through its container.
    #[test]
    fn a_nested_reference_touches_refs() {
        let group = FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![image()])
            .build();
        let slides = FieldDefinition::builder("slides", FieldType::Array)
            .fields(vec![image()])
            .build();
        let fields = vec![group, slides];

        assert!(data_touches_refs(&fields, &data("meta__image"), ""));
        assert!(data_touches_refs(&fields, &data("slides"), ""));
        assert!(!data_touches_refs(&fields, &data("other"), ""));
    }
}
