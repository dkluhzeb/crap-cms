//! Resolve dot-notation filter paths to SQL representations
//! ([`super::types::ResolvedFilter`]).

use anyhow::{Result, anyhow, bail};

use crate::core::{
    BLOCK_TYPE_KEY, FieldDefinition, FieldType, find_field, flatten_array_sub_fields,
};
use crate::db::query::filter::elements::ListLeaf;
use crate::db::query::helpers::{join_table, qualified_ident};
use crate::db::query::{column_read_expr, is_valid_identifier, qualified_column_read_expr};
use crate::db::{DbConnection, LocaleContext};

use super::blocks::walk_block_fields;
use super::lookup::lookup_column_field;
use super::types::{ResolvedFilter, SubqueryCondition};

/// Resolve a dot-notation filter field to its SQL representation.
///
/// Non-dot fields return [`ResolvedFilter::Column`] carrying the column's read
/// expression — the fallback `COALESCE` for a localized column, so the filter
/// compares what the SELECT returns. Dot fields are routed
/// based on the root field type:
/// - **Array** → subquery with typed column on join table
/// - **Blocks** → subquery with `json_extract` (and `json_each` for nesting)
/// - **Relationship / Upload** (has-many) → subquery on the junction's `related_id`
///
/// `locale_ctx` drives per-locale filtering on junction tables: when the
/// target array/blocks/relationship field is `localized` and the query is
/// scoped to a single locale (`Single` or `Default`), the returned
/// [`ResolvedFilter::Subquery`] carries a `locale_constraint` so the EXISTS
/// subquery adds a `_locale = ?` clause. `LocaleMode::All` leaves the
/// constraint empty (match rows in any locale).
pub(in crate::db::query::filter) fn resolve_filter(
    conn: &dyn DbConnection,
    field: &str,
    slug: &str,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
) -> Result<ResolvedFilter> {
    if !field.contains('.') {
        return resolve_column(field, slug, fields, locale_ctx);
    }

    // Guarded by early return above: field.contains('.') is true here
    let dot_pos = field.find('.').expect("dot checked above");
    let root = &field[..dot_pos];
    let rest = &field[dot_pos + 1..];

    let field_def = find_field(root, fields)
        .ok_or_else(|| anyhow!("Unknown field '{root}' in filter path '{field}'"))?;

    // The junction table has a `_locale` column iff the container field is
    // itself localized. Transparent layout wrappers (Row/Collapsible/Tabs)
    // do not carry localization, so we only need to check `field_def.localized`.
    // Nested containers inside a localized Group use `{group}__{array}` dot
    // notation that does not route here (resolve_filter expects the root to
    // be a top-level Array/Blocks/Relationship), so inherited Group locale
    // does not apply at this call site.
    let has_locale_col = field_def.localized && locale_ctx.is_some_and(|c| c.config.is_enabled());
    let locale_constraint = has_locale_col
        .then(|| locale_ctx.and_then(subquery_locale))
        .flatten();

    let ctx = SubFilterCtx {
        conn,
        root,
        rest,
        field,
        slug,
        field_def,
        join_table: join_table(slug, root),
        locale_constraint,
    };

    match field_def.field_type {
        FieldType::Array => resolve_array_filter(ctx),
        FieldType::Blocks => resolve_blocks_filter(ctx),
        FieldType::Relationship | FieldType::Upload => resolve_relationship_filter(ctx),
        _ => bail!(
            "Field '{}' (type {:?}) does not support sub-field filtering",
            root,
            field_def.field_type
        ),
    }
}

/// Resolve a parent-table column filter.
///
/// Localized columns resolve HERE, at the single point the parent-table
/// comparand is built — the SELECT, the sort and the keyset take the same
/// expression, so a filter matches the values the read returns. A scalar
/// has-many column is read qualified by its table: its filter expands the list
/// in a subquery, whose own columns would otherwise shadow the bare name.
fn resolve_column(
    field: &str,
    slug: &str,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
) -> Result<ResolvedFilter> {
    let leaf = lookup_column_field(field, fields);
    let list = leaf.and_then(ListLeaf::of);

    let expr = if list.is_some() {
        qualified_column_read_expr(slug, field, fields, locale_ctx)?
    } else {
        column_read_expr(field, fields, locale_ctx)?
    };

    Ok(ResolvedFilter::Column {
        expr,
        field_type: leaf.map(|f| f.field_type.clone()),
        list,
    })
}

/// Resolved context for sub-field filter helpers. Built once in
/// [`resolve_filter`] and consumed by the per-type resolver.
struct SubFilterCtx<'a> {
    conn: &'a dyn DbConnection,
    root: &'a str,
    rest: &'a str,
    field: &'a str,
    slug: &'a str,
    field_def: &'a FieldDefinition,
    join_table: String,
    locale_constraint: Option<String>,
}

/// Pick the locale string to use as a `_locale = ?` constraint for a subquery:
/// the locale a single-locale read takes, or `None` for an all-locales read
/// (match across all locales).
fn subquery_locale(ctx: &LocaleContext) -> Option<String> {
    ctx.read_locale().map(|read| read.locale.to_string())
}

fn resolve_array_filter(ctx: SubFilterCtx<'_>) -> Result<ResolvedFilter> {
    for seg in ctx.rest.split('.') {
        if !is_valid_identifier(seg) {
            bail!("Invalid segment '{}' in filter path '{}'", seg, ctx.field);
        }
    }
    if let Some(dot) = ctx.rest.find('.') {
        // Dotted path inside array — check if first segment is a Group sub-field.
        let first_seg = &ctx.rest[..dot];
        let remaining = &ctx.rest[dot + 1..];
        let sub_def = array_sub_field(ctx.field_def, first_seg);
        match sub_def.map(|f| &f.field_type) {
            Some(FieldType::Group) => {
                // Group sub-fields in arrays are stored as JSON TEXT columns.
                // Access nested values via json_extract, reading the column
                // qualified by its join table: a has-many list inside the group
                // is expanded in a subquery whose own columns would shadow it.
                let column = qualified_ident(Some(ctx.join_table.as_str()), first_seg);
                let extract_expr = ctx.conn.json_extract_expr(&column, remaining);
                let leaf = sub_def.and_then(|g| find_field(remaining, &g.fields));
                Ok(ResolvedFilter::Subquery {
                    join_table: ctx.join_table,
                    parent_table: ctx.slug.to_string(),
                    condition: SubqueryCondition::Json {
                        each_joins: vec![],
                        extract_expr,
                        field_type: leaf.map(|f| f.field_type.clone()),
                        list: leaf.and_then(ListLeaf::of),
                    },
                    locale_constraint: ctx.locale_constraint,
                })
            }
            _ => bail!(
                "Nested dot path '{}' in array '{}': only Group sub-fields support nested filtering",
                ctx.rest,
                ctx.root
            ),
        }
    } else {
        // Simple sub-field — direct typed column on join table.
        let leaf = array_sub_field(ctx.field_def, ctx.rest);
        Ok(ResolvedFilter::Subquery {
            join_table: ctx.join_table,
            parent_table: ctx.slug.to_string(),
            condition: SubqueryCondition::Column {
                col: ctx.rest.to_string(),
                field_type: leaf.map(|f| f.field_type.clone()),
                list: leaf.and_then(ListLeaf::of),
            },
            locale_constraint: ctx.locale_constraint,
        })
    }
}

/// The sub-field `name` of an array's rows. Layout wrappers are transparent
/// in a row, the way its join table's columns are.
fn array_sub_field<'a>(array: &'a FieldDefinition, name: &str) -> Option<&'a FieldDefinition> {
    flatten_array_sub_fields(&array.fields)
        .into_iter()
        .find(|f| f.name == name)
}

fn resolve_blocks_filter(ctx: SubFilterCtx<'_>) -> Result<ResolvedFilter> {
    if ctx.rest == BLOCK_TYPE_KEY {
        return Ok(ResolvedFilter::Subquery {
            join_table: ctx.join_table,
            parent_table: ctx.slug.to_string(),
            condition: SubqueryCondition::BlockType,
            locale_constraint: ctx.locale_constraint,
        });
    }

    let rest_parts: Vec<&str> = ctx.rest.split('.').collect();
    for seg in &rest_parts {
        if !is_valid_identifier(seg) && *seg != BLOCK_TYPE_KEY {
            bail!("Invalid segment '{}' in filter path '{}'", seg, ctx.field);
        }
    }
    let (each_joins, extract_expr, field_type, list) = walk_block_fields(
        ctx.conn,
        &rest_parts,
        &ctx.field_def.blocks,
        &ctx.join_table,
    )?;

    Ok(ResolvedFilter::Subquery {
        join_table: ctx.join_table,
        parent_table: ctx.slug.to_string(),
        condition: SubqueryCondition::Json {
            each_joins,
            extract_expr,
            field_type,
            list,
        },
        locale_constraint: ctx.locale_constraint,
    })
}

fn resolve_relationship_filter(ctx: SubFilterCtx<'_>) -> Result<ResolvedFilter> {
    let Some(rc) = ctx.field_def.relationship.as_ref() else {
        bail!(
            "Relationship field '{}' missing relationship config",
            ctx.root
        );
    };

    if !rc.has_many {
        bail!(
            "Has-one relationship '{}' does not use dot notation for filtering",
            ctx.root
        );
    }

    if ctx.rest != "id" {
        bail!(
            "Has-many relationship '{}' can only be filtered by '.id', got '.{}'",
            ctx.root,
            ctx.rest
        );
    }

    Ok(ResolvedFilter::Subquery {
        join_table: ctx.join_table,
        parent_table: ctx.slug.to_string(),
        condition: SubqueryCondition::RelatedId,
        locale_constraint: ctx.locale_constraint,
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
    use crate::config::LocaleConfig;
    use crate::core::RelationshipConfig;
    use crate::db::LocaleMode;
    use crate::db::query::filter::resolve::test_helpers::*;

    #[test]
    fn resolve_filter_no_dots_returns_column() {
        let (_dir, conn) = test_conn();
        let resolved = resolve_filter(&conn, "status", "posts", &[], None).unwrap();
        match resolved {
            ResolvedFilter::Column {
                expr,
                field_type,
                list,
            } => {
                assert_eq!(expr, "\"status\"");
                assert_eq!(field_type, None);
                assert!(list.is_none());
            }
            other => panic!("Expected Column, got {other:?}"),
        }
    }

    /// A localized column resolves to the same fallback expression the SELECT
    /// reads it through, so a filter matches the value the listing shows.
    #[test]
    fn resolve_filter_reads_a_localized_column_with_its_fallback() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_field("title", FieldType::Text, true)];
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        };

        let resolved = resolve_filter(&conn, "title", "posts", &fields, Some(&ctx)).unwrap();

        match resolved {
            ResolvedFilter::Column { expr, .. } => {
                assert_eq!(expr, "COALESCE(\"title__de\", \"title__en\")");
            }
            other => panic!("Expected Column, got {other:?}"),
        }
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
                locale_constraint,
            } => {
                assert_eq!(join_table, "posts_items");
                assert_eq!(parent_table, "posts");
                assert_eq!(locale_constraint, None);
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
        match resolved {
            ResolvedFilter::Subquery { condition, .. } => match condition {
                SubqueryCondition::Json {
                    each_joins,
                    extract_expr,
                    field_type,
                    ..
                } => {
                    assert!(each_joins.is_empty());
                    assert_eq!(
                        extract_expr,
                        "json_extract(\"posts_items\".\"address\", '$.city')"
                    );
                    assert_eq!(field_type, Some(FieldType::Text));
                }
                other => panic!("Expected Json, got {other:?}"),
            },
            other => panic!("Expected Subquery, got {other:?}"),
        }
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
        match resolved {
            ResolvedFilter::Subquery { condition, .. } => match condition {
                SubqueryCondition::Json {
                    each_joins,
                    extract_expr,
                    field_type,
                    ..
                } => {
                    assert!(each_joins.is_empty());
                    assert_eq!(extract_expr, "json_extract(data, '$.body')");
                    assert_eq!(field_type, Some(FieldType::Textarea));
                }
                other => panic!("Expected Json, got {other:?}"),
            },
            other => panic!("Expected Subquery, got {other:?}"),
        }
    }

    #[test]
    fn resolve_filter_has_many_relationship() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_has_many_field("tags", "tags")];
        let resolved = resolve_filter(&conn, "tags.id", "posts", &fields, None).unwrap();
        match resolved {
            ResolvedFilter::Subquery {
                join_table,
                condition,
                ..
            } => {
                assert_eq!(join_table, "posts_tags");
                assert!(matches!(condition, SubqueryCondition::RelatedId));
            }
            other => panic!("Expected Subquery, got {other:?}"),
        }
    }

    /// Regression: a has-many upload keeps its ids in a junction table exactly
    /// like a has-many relationship, and the query validation accepts
    /// `gallery.id` — but the resolver only routed relationships, so the
    /// filter failed with "does not support sub-field filtering".
    #[test]
    fn resolve_filter_has_many_upload() {
        let (_dir, conn) = test_conn();
        let fields = vec![
            FieldDefinition::builder("gallery", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", true))
                .build(),
        ];

        let resolved = resolve_filter(&conn, "gallery.id", "posts", &fields, None).unwrap();

        match resolved {
            ResolvedFilter::Subquery {
                join_table,
                condition,
                ..
            } => {
                assert_eq!(join_table, "posts_gallery");
                assert!(matches!(condition, SubqueryCondition::RelatedId));
            }
            other => panic!("Expected Subquery, got {other:?}"),
        }
    }

    /// A scalar has-many column is a list: its filter quantifies over the
    /// elements, reading the column qualified by its table so the element
    /// expansion cannot shadow it.
    #[test]
    fn resolve_filter_scalar_has_many_column_is_a_qualified_list() {
        let (_dir, conn) = test_conn();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .build(),
        ];

        let resolved = resolve_filter(&conn, "tags", "posts", &fields, None).unwrap();

        match resolved {
            ResolvedFilter::Column {
                expr,
                field_type,
                list,
            } => {
                assert_eq!(expr, "\"posts\".\"tags\"");
                assert_eq!(field_type, Some(FieldType::Text));
                assert_eq!(list, Some(ListLeaf::Scalar(FieldType::Text)));
            }
            other => panic!("Expected Column, got {other:?}"),
        }
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
    fn resolve_filter_has_many_rejects_non_id() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_has_many_field("tags", "tags")];
        let result = resolve_filter(&conn, "tags.name", "posts", &fields, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("only be filtered by '.id'")
        );
    }

    #[test]
    fn resolve_filter_has_one_relationship_rejects_dot() {
        let (_dir, conn) = test_conn();
        let fields = vec![
            FieldDefinition::builder("author", FieldType::Relationship)
                .relationship(RelationshipConfig::new("users", false))
                .build(),
        ];
        let result = resolve_filter(&conn, "author.name", "posts", &fields, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Has-one"));
    }

    #[test]
    fn resolve_filter_unsupported_field_type() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_field("title", FieldType::Text, false)];
        let result = resolve_filter(&conn, "title.sub", "posts", &fields, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("does not support sub-field filtering")
        );
    }

    #[test]
    fn resolve_filter_relationship_missing_config() {
        let (_dir, conn) = test_conn();
        let fields = vec![FieldDefinition::builder("tags", FieldType::Relationship).build()];
        let result = resolve_filter(&conn, "tags.id", "posts", &fields, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing relationship config")
        );
    }

    #[test]
    fn resolve_filter_unknown_root_field() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_field("title", FieldType::Text, false)];
        let result = resolve_filter(&conn, "nonexistent.sub", "posts", &fields, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown field"));
    }

    #[test]
    fn resolve_filter_array_nested_non_group_error() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];
        let result = resolve_filter(&conn, "items.name.deep", "posts", &fields, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Nested dot path"));
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
