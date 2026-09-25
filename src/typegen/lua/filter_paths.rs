//! The dotted filter paths a query may name beside the flat columns: a
//! group's value in its dotted spelling, and every path into the rows of an
//! array, blocks or has-many reference field — the grammar the filter
//! resolver accepts (`db::query::filter::resolve`), walked over the schema.

use std::collections::HashSet;

use crate::core::{
    BLOCK_TYPE_KEY, BlockDefinition, CollectionDefinition, FieldChildren, FieldDefinition,
    FieldType, field_children, find_field, flatten_array_sub_fields,
};

/// The key of a top-level array or blocks row's own id.
const ROW_ID: &str = "id";

/// The dotted spelling of a group's value column (`seo__meta_title` →
/// `seo.meta_title`), which filters and sorts accept alike. `None` for a
/// column that is not below a group.
pub(super) fn dotted_group_column(col: &CollectionDefinition, column: &str) -> Option<String> {
    let (root, _) = column.split_once("__")?;
    let field = find_field(root, &col.fields)?;

    (field.field_type == FieldType::Group).then(|| column.replace("__", "."))
}

/// Every dotted path into the rows of `fields`' array, blocks and has-many
/// reference fields — at the top level or inside groups — in schema order,
/// each once.
pub(super) fn row_filter_paths(fields: &[FieldDefinition]) -> Vec<String> {
    let mut paths = Vec::new();
    collect_containers(fields, "", &mut paths);

    let mut seen = HashSet::new();
    paths.retain(|p| seen.insert(p.clone()));

    paths
}

/// `prefix.name`, or `name` at the top level.
fn join_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        return name.to_string();
    }

    format!("{prefix}.{name}")
}

/// The join-table fields of `fields` below the group path `prefix`, each with
/// the paths into its rows.
fn collect_containers(fields: &[FieldDefinition], prefix: &str, out: &mut Vec<String>) {
    for field in fields {
        let path = join_path(prefix, &field.name);

        match field_children(field) {
            FieldChildren::Wrapper(sub) => collect_containers(sub, prefix, out),
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    collect_containers(&tab.fields, prefix, out);
                }
            }
            FieldChildren::Group(sub) => collect_containers(sub, &path, out),
            FieldChildren::Array(sub) => {
                out.push(join_path(&path, ROW_ID));
                row_paths(sub, &path, out);
            }
            FieldChildren::Blocks(blocks) => {
                out.push(join_path(&path, ROW_ID));
                out.push(join_path(&path, BLOCK_TYPE_KEY));
                block_paths(blocks, &path, out);
            }
            FieldChildren::Leaf if field.is_has_many_reference() => {
                out.push(join_path(&path, ROW_ID));
            }
            FieldChildren::Leaf => {}
        }
    }
}

/// The paths to every value of a row holding `fields`, below `prefix`: a
/// value field, and — through a group, a nested array or nested blocks stored
/// as JSON — the values inside it, at any depth. A nested row has no
/// filterable id; a nested blocks row has its `_block_type`.
fn row_paths(fields: &[FieldDefinition], prefix: &str, out: &mut Vec<String>) {
    for field in flatten_array_sub_fields(fields) {
        let path = join_path(prefix, &field.name);

        match field_children(field) {
            FieldChildren::Group(sub) | FieldChildren::Array(sub) => row_paths(sub, &path, out),
            FieldChildren::Blocks(blocks) => {
                out.push(join_path(&path, BLOCK_TYPE_KEY));
                block_paths(blocks, &path, out);
            }
            FieldChildren::Leaf if field.field_type.is_writable() => out.push(path),
            FieldChildren::Leaf | FieldChildren::Wrapper(_) | FieldChildren::Tabs(_) => {}
        }
    }
}

/// The value paths of a blocks row below `prefix`: every block type's.
fn block_paths(blocks: &[BlockDefinition], prefix: &str, out: &mut Vec<String>) {
    for block in blocks {
        row_paths(&block.fields, prefix, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::RelationshipConfig;

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    #[test]
    fn group_columns_have_a_dotted_spelling() {
        let mut col = CollectionDefinition::new("pages");
        col.fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("social", FieldType::Group)
                        .fields(vec![text("handle")])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("published_at", FieldType::Date)
                .timezone(true)
                .build(),
        ];

        assert_eq!(
            dotted_group_column(&col, "seo__social__handle").as_deref(),
            Some("seo.social.handle")
        );
        assert_eq!(dotted_group_column(&col, "published_at_tz"), None);
        assert_eq!(dotted_group_column(&col, "title"), None);
    }

    /// Every container kind the resolver routes: an array's row id and
    /// values (through layout wrappers, a JSON group, a nested array and
    /// nested blocks), a blocks field's id, type and every block type's
    /// values, a has-many reference's `.id`, and a join table held by a
    /// group. A has-one reference and a virtual join yield nothing.
    #[test]
    fn row_paths_follow_the_resolver_grammar() {
        let fields = vec![
            FieldDefinition::builder("variants", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("layout", FieldType::Row)
                        .fields(vec![text("color")])
                        .build(),
                    FieldDefinition::builder("dims", FieldType::Group)
                        .fields(vec![text("width")])
                        .build(),
                    FieldDefinition::builder("sizes", FieldType::Array)
                        .fields(vec![text("label")])
                        .build(),
                    FieldDefinition::builder("parts", FieldType::Blocks)
                        .blocks(vec![BlockDefinition::new("bolt", vec![text("size")])])
                        .build(),
                    FieldDefinition::builder("mentions", FieldType::Join).build(),
                ])
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![
                    BlockDefinition::new("hero", vec![text("heading")]),
                    BlockDefinition::new("quote", vec![text("heading"), text("cite")]),
                ])
                .build(),
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
            FieldDefinition::builder("author", FieldType::Relationship)
                .relationship(RelationshipConfig::new("users", false))
                .build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("links", FieldType::Array)
                        .fields(vec![text("url")])
                        .build(),
                ])
                .build(),
        ];

        assert_eq!(
            row_filter_paths(&fields),
            [
                "variants.id",
                "variants.color",
                "variants.dims.width",
                "variants.sizes.label",
                "variants.parts._block_type",
                "variants.parts.size",
                "content.id",
                "content._block_type",
                "content.heading",
                "content.cite",
                "tags.id",
                "seo.links.id",
                "seo.links.url",
            ]
        );
    }
}
