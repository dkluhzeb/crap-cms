//! Resolve a filter path into the rows of an array or blocks field — a typed
//! column of an array row, or a value inside a row's JSON at any depth.

use anyhow::{Result, bail};

use crate::core::{
    BLOCK_TYPE_KEY, FieldChildren, FieldDefinition, field_children, flatten_array_sub_fields,
};
use crate::db::query::filter::elements::ListLeaf;
use crate::db::query::{helpers::qualified_ident, is_valid_identifier};

use super::json_walk::JsonWalk;
use super::path::SubFilterCtx;
use super::types::{ResolvedFilter, SubqueryCondition};

/// The join-table column every array and blocks row carries beside its
/// values: the row's own id, which a write round-trips. Only a top-level row
/// has one — a row nested in another row's JSON is not addressed by id.
pub(in crate::db::query::filter) const ROW_ID: &str = "id";

pub(super) fn resolve_array_filter(ctx: SubFilterCtx<'_>) -> Result<ResolvedFilter> {
    for seg in ctx.rest.split('.') {
        if !is_valid_identifier(seg) {
            bail!("Invalid segment '{}' in filter path '{}'", seg, ctx.field);
        }
    }

    let Some((first_seg, remaining)) = ctx.rest.split_once('.') else {
        return resolve_array_column(ctx);
    };

    let Some(container) = array_sub_field(ctx.field_def, first_seg) else {
        bail!("Unknown sub-field '{}' in array '{}'", first_seg, ctx.root);
    };

    // A group, nested array or nested blocks sub-field is stored as JSON in
    // its column; the path below it walks that JSON like a block row's. The
    // column is read qualified by its join table: a list at the leaf is
    // expanded in a subquery whose own columns would shadow it.
    let column = qualified_ident(Some(ctx.join_table.as_str()), first_seg);
    let segments: Vec<&str> = remaining.split('.').collect();
    let leaves = JsonWalk::array_column(ctx.conn, column, container)?.walk(ctx.conn, &segments)?;

    Ok(ResolvedFilter::Subquery {
        join_table: ctx.join_table,
        parent_table: ctx.slug.to_string(),
        condition: SubqueryCondition::Json(leaves),
        rows_locale: ctx.rows_locale,
    })
}

/// A plain array sub-field — a typed column on the join table. A name the
/// rows do not have is rejected here, before it reaches SQL as a column, and
/// so is a container (a group, a nested array or blocks), whose column holds
/// JSON rather than a value, and a field storing no value at all (a join).
fn resolve_array_column(ctx: SubFilterCtx<'_>) -> Result<ResolvedFilter> {
    let leaf = array_sub_field(ctx.field_def, ctx.rest);

    match leaf {
        None if ctx.rest != ROW_ID => {
            bail!("Unknown sub-field '{}' in array '{}'", ctx.rest, ctx.root);
        }
        Some(f) if !matches!(field_children(f), FieldChildren::Leaf) => {
            bail!(
                "Filter path must end on a value field, not the container '{}'",
                ctx.rest
            );
        }
        Some(f) if !f.field_type.is_writable() => {
            bail!(
                "Field '{}' (type {:?}) stores no value to filter on",
                ctx.rest,
                f.field_type
            );
        }
        _ => {}
    }

    Ok(ResolvedFilter::Subquery {
        join_table: ctx.join_table,
        parent_table: ctx.slug.to_string(),
        condition: SubqueryCondition::Column {
            col: ctx.rest.to_string(),
            field_type: leaf.map(|f| f.field_type.clone()),
            list: leaf.and_then(ListLeaf::of),
        },
        rows_locale: ctx.rows_locale,
    })
}

/// The sub-field `name` of an array's rows. Layout wrappers are transparent
/// in a row, the way its join table's columns are.
fn array_sub_field<'a>(array: &'a FieldDefinition, name: &str) -> Option<&'a FieldDefinition> {
    flatten_array_sub_fields(&array.fields)
        .into_iter()
        .find(|f| f.name == name)
}

pub(super) fn resolve_blocks_filter(ctx: SubFilterCtx<'_>) -> Result<ResolvedFilter> {
    if ctx.rest == BLOCK_TYPE_KEY {
        return Ok(ResolvedFilter::Subquery {
            join_table: ctx.join_table,
            parent_table: ctx.slug.to_string(),
            condition: SubqueryCondition::BlockType,
            rows_locale: ctx.rows_locale,
        });
    }

    if ctx.rest == ROW_ID {
        return Ok(ResolvedFilter::Subquery {
            join_table: ctx.join_table,
            parent_table: ctx.slug.to_string(),
            condition: SubqueryCondition::Column {
                col: ROW_ID.to_string(),
                field_type: None,
                list: None,
            },
            rows_locale: ctx.rows_locale,
        });
    }

    let rest_parts: Vec<&str> = ctx.rest.split('.').collect();
    for seg in &rest_parts {
        if !is_valid_identifier(seg) && *seg != BLOCK_TYPE_KEY {
            bail!("Invalid segment '{}' in filter path '{}'", seg, ctx.field);
        }
    }

    let leaves =
        JsonWalk::block_row(&ctx.join_table, &ctx.field_def.blocks).walk(ctx.conn, &rest_parts)?;

    Ok(ResolvedFilter::Subquery {
        join_table: ctx.join_table,
        parent_table: ctx.slug.to_string(),
        condition: SubqueryCondition::Json(leaves),
        rows_locale: ctx.rows_locale,
    })
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use super::*;
    use crate::core::FieldType;
    use crate::db::query::filter::resolve::{JsonLeaf, resolve_filter, test_helpers::*};

    /// The one reading of a JSON row path that holds in every row.
    fn single_json(resolved: ResolvedFilter) -> JsonLeaf {
        let ResolvedFilter::Subquery {
            condition: SubqueryCondition::Json(leaves),
            ..
        } = resolved
        else {
            panic!("Expected a Json subquery, got {resolved:?}");
        };

        let [leaf] = <[JsonLeaf; 1]>::try_from(leaves).expect("exactly one reading");
        assert!(leaf.is_unconditional(), "unexpected block-type step");

        leaf
    }

    #[test]
    fn resolve_filter_array_subfield() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];
        let resolved = resolve_filter(&conn, "items.name", "posts", &fields, None).unwrap();
        match resolved {
            ResolvedFilter::Subquery {
                join_table,
                parent_table,
                condition,
                rows_locale,
            } => {
                assert_eq!(join_table, "posts_items");
                assert_eq!(parent_table, "posts");
                assert_eq!(rows_locale, None);
                match condition {
                    SubqueryCondition::Column {
                        col,
                        field_type,
                        list,
                    } => {
                        assert_eq!(col, "name");
                        assert_eq!(field_type, Some(FieldType::Text));
                        assert!(list.is_none());
                    }
                    other => panic!("Expected Column, got {other:?}"),
                }
            }
            other => panic!("Expected Subquery, got {other:?}"),
        }
    }

    #[test]
    fn resolve_filter_array_group_in_array() {
        let (_dir, conn) = test_conn();
        let mut addr = make_field("address", FieldType::Group, false);
        addr.fields = vec![make_field("city", FieldType::Text, false)];
        let fields = vec![make_array_field("items", vec![addr])];

        let resolved = resolve_filter(&conn, "items.address.city", "posts", &fields, None).unwrap();
        let leaf = single_json(resolved);

        assert!(leaf.steps.is_empty());
        assert_eq!(
            leaf.extract_expr,
            "json_extract(\"posts_items\".\"address\", '$.city')"
        );
        assert_eq!(leaf.field_type, Some(FieldType::Text));
    }

    #[test]
    fn resolve_filter_block_type() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_blocks_field("content", vec![])];
        let resolved =
            resolve_filter(&conn, "content._block_type", "posts", &fields, None).unwrap();
        match resolved {
            ResolvedFilter::Subquery { condition, .. } => {
                assert!(matches!(condition, SubqueryCondition::BlockType));
            }
            other => panic!("Expected Subquery, got {other:?}"),
        }
    }

    #[test]
    fn resolve_filter_block_scalar() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_blocks_field(
            "content",
            vec![make_block_def(
                "paragraph",
                vec![make_field("body", FieldType::Textarea, false)],
            )],
        )];
        let resolved = resolve_filter(&conn, "content.body", "posts", &fields, None).unwrap();
        let leaf = single_json(resolved);

        assert!(leaf.steps.is_empty());
        assert_eq!(
            leaf.extract_expr,
            "json_extract(posts_content.data, '$.body')"
        );
        assert_eq!(leaf.field_type, Some(FieldType::Textarea));
    }

    /// A scalar has-many sub-field of an array row is a list too.
    #[test]
    fn resolve_filter_scalar_has_many_array_sub_field_is_a_list() {
        let (_dir, conn) = test_conn();
        let tags = FieldDefinition::builder("tags", FieldType::Number)
            .has_many(true)
            .build();
        let fields = vec![make_array_field("items", vec![tags])];

        let resolved = resolve_filter(&conn, "items.tags", "posts", &fields, None).unwrap();

        match resolved {
            ResolvedFilter::Subquery {
                condition: SubqueryCondition::Column { list, .. },
                ..
            } => assert_eq!(list, Some(ListLeaf::Scalar(FieldType::Number))),
            other => panic!("Expected a Column subquery, got {other:?}"),
        }
    }

    /// A has-many relationship sub-field of an array row stores its id list
    /// in its column, which its filter reads as a list of ids.
    #[test]
    fn resolve_filter_has_many_reference_array_sub_field_is_a_list() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_array_field(
            "items",
            vec![make_has_many_field("related", "tags")],
        )];

        let resolved = resolve_filter(&conn, "items.related", "posts", &fields, None).unwrap();

        match resolved {
            ResolvedFilter::Subquery {
                condition: SubqueryCondition::Column { list, .. },
                ..
            } => assert_eq!(list, Some(ListLeaf::References { polymorphic: false })),
            other => panic!("Expected a Column subquery, got {other:?}"),
        }
    }

    /// Regression: an array sub-field inside a layout row was looked up among
    /// the array's direct sub-fields only, so its filter lost the field's type
    /// — a number compared as text. Layout wrappers are transparent in a row.
    #[test]
    fn resolve_filter_array_sub_field_inside_a_row_keeps_its_type() {
        let (_dir, conn) = test_conn();
        let row = FieldDefinition::builder("layout", FieldType::Row)
            .fields(vec![make_field("width", FieldType::Number, false)])
            .build();
        let fields = vec![make_array_field("items", vec![row])];

        let resolved = resolve_filter(&conn, "items.width", "posts", &fields, None).unwrap();

        match resolved {
            ResolvedFilter::Subquery {
                condition: SubqueryCondition::Column { field_type, .. },
                ..
            } => assert_eq!(field_type, Some(FieldType::Number)),
            other => panic!("Expected a Column subquery, got {other:?}"),
        }
    }

    #[test]
    fn resolve_filter_array_sub_path_into_a_value_error() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];
        let result = resolve_filter(&conn, "items.name.deep", "posts", &fields, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("'name' has no sub-fields")
        );
    }

    /// A sub-field the array's rows do not have is rejected before it reaches
    /// SQL as a column — a typed error naming the path, not a backend failure.
    #[test]
    fn resolve_filter_unknown_array_sub_field_is_a_typed_error() {
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];

        let (field, message) = path_error(&fields, "items.nope");

        assert_eq!(field, "items.nope");
        assert!(message.contains("Unknown sub-field 'nope'"), "{message}");
    }

    /// The row's own id is a column of every array row.
    #[test]
    fn resolve_filter_array_row_id_is_a_column() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];

        let resolved = resolve_filter(&conn, "items.id", "posts", &fields, None).unwrap();

        match resolved {
            ResolvedFilter::Subquery {
                condition: SubqueryCondition::Column { col, .. },
                ..
            } => assert_eq!(col, "id"),
            other => panic!("Expected Column subquery, got {other:?}"),
        }
    }

    /// Regression: a blocks row's own id was rejected while an array row's was
    /// accepted, though both rows round-trip one. Both are the row table's
    /// `id` column; a row nested in another row's JSON has none to filter.
    #[test]
    fn resolve_filter_blocks_row_id_is_a_column() {
        let (_dir, conn) = test_conn();
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = vec![make_block_def("quote", vec![])];
        let fields = vec![make_blocks_field(
            "content",
            vec![make_block_def("text", vec![nested])],
        )];

        let resolved = resolve_filter(&conn, "content.id", "posts", &fields, None).unwrap();

        match resolved {
            ResolvedFilter::Subquery {
                condition: SubqueryCondition::Column { col, .. },
                join_table,
                ..
            } => {
                assert_eq!(col, "id");
                assert_eq!(join_table, "posts_content");
            }
            other => panic!("Expected Column subquery, got {other:?}"),
        }

        assert!(resolve_filter(&conn, "content.nested.id", "posts", &fields, None).is_err());
        assert!(resolve_filter(&conn, "content._order", "posts", &fields, None).is_err());
        assert!(resolve_filter(&conn, "content.parent_id", "posts", &fields, None).is_err());
    }

    /// Only the row id is a filterable row column; the bookkeeping columns
    /// stay out of reach on arrays too.
    #[test]
    fn resolve_filter_array_bookkeeping_columns_are_rejected() {
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];

        for path in ["items._order", "items.parent_id", "items._locale"] {
            let (field, _) = path_error(&fields, path);

            assert_eq!(field, path);
        }
    }

    /// Regression: an array row's nested array and nested blocks were not
    /// filterable (only a group sub-field was), while a blocks row reached the
    /// same shapes at any depth. Both now walk the row's JSON the same way.
    #[test]
    fn resolve_filter_array_row_reaches_nested_arrays_and_blocks() {
        let (_dir, conn) = test_conn();
        let sizes = make_array_field("sizes", vec![make_field("label", FieldType::Text, false)]);
        let sections = make_blocks_field(
            "sections",
            vec![make_block_def(
                "grid",
                vec![make_field("cell", FieldType::Number, false)],
            )],
        );
        let fields = vec![make_array_field("items", vec![sizes, sections])];

        for (path, expected_expr, expected_type) in [
            (
                "items.sizes.label",
                "json_extract(j0.value, '$.label')",
                FieldType::Text,
            ),
            (
                "items.sections.cell",
                "json_extract(j0.value, '$.cell')",
                FieldType::Number,
            ),
            (
                "items.sections._block_type",
                "json_extract(j0.value, '$._block_type')",
                FieldType::Text,
            ),
        ] {
            let resolved = resolve_filter(&conn, path, "posts", &fields, None).unwrap();
            let leaf = single_json(resolved);

            assert_eq!(leaf.each_joins().len(), 1, "{path}");
            assert_eq!(leaf.extract_expr, expected_expr, "{path}");
            assert_eq!(leaf.field_type, Some(expected_type), "{path}");
        }
    }

    /// Groups nest inside an array row's group; the leaf keeps its type, and a
    /// container at the end of the path is refused.
    #[test]
    fn resolve_filter_nested_group_in_array_row() {
        let (_dir, conn) = test_conn();
        let mut geo = make_field("geo", FieldType::Group, false);
        geo.fields = vec![make_field("lat", FieldType::Number, false)];
        let mut address = make_field("address", FieldType::Group, false);
        address.fields = vec![geo];
        let fields = vec![make_array_field("items", vec![address])];

        let resolved =
            resolve_filter(&conn, "items.address.geo.lat", "posts", &fields, None).unwrap();

        assert_eq!(single_json(resolved).field_type, Some(FieldType::Number));

        let (_, message) = path_error(&fields, "items.address.geo");
        assert!(message.contains("not the container 'geo'"), "{message}");
    }

    /// Regression: a join field among an array's sub-fields was accepted as a
    /// filter column, though it stores nothing; top level refuses it.
    #[test]
    fn resolve_filter_join_array_sub_field_is_refused() {
        let fields = vec![make_array_field(
            "items",
            vec![make_field("posts", FieldType::Join, false)],
        )];

        let (field, message) = path_error(&fields, "items.posts");

        assert_eq!(field, "items.posts");
        assert!(message.contains("stores no value"), "{message}");
    }

    /// Block types defining a name differently resolve to one reading per
    /// block type, each for its own rows.
    #[test]
    fn resolve_filter_block_types_defining_a_name_differently_read_per_type() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_blocks_field(
            "content",
            vec![
                make_block_def("stat", vec![make_field("score", FieldType::Number, false)]),
                make_block_def("note", vec![make_field("score", FieldType::Text, false)]),
            ],
        )];

        let resolved = resolve_filter(&conn, "content.score", "posts", &fields, None).unwrap();

        let ResolvedFilter::Subquery {
            condition: SubqueryCondition::Json(leaves),
            ..
        } = resolved
        else {
            panic!("Expected a Json subquery, got {resolved:?}");
        };
        let types: Vec<Option<FieldType>> = leaves.iter().map(|l| l.field_type.clone()).collect();

        // One reading per declaring type, then the absent reading for rows of
        // any other type.
        assert_eq!(
            types,
            vec![Some(FieldType::Number), Some(FieldType::Text), None]
        );
        assert!(leaves.iter().all(|leaf| !leaf.is_unconditional()));
        assert!(leaves[2].reads_absent());
    }

    #[test]
    fn resolve_filter_array_invalid_segment() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];
        let result = resolve_filter(&conn, "items.bad field", "posts", &fields, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid segment"));
    }

    #[test]
    fn resolve_filter_blocks_invalid_segment() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_blocks_field(
            "content",
            vec![make_block_def(
                "text",
                vec![make_field("body", FieldType::Textarea, false)],
            )],
        )];
        let result = resolve_filter(&conn, "content.bad field", "posts", &fields, None);
        assert!(result.is_err());
    }
}
