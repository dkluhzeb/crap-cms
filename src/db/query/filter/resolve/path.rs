//! Resolve dot-notation filter paths to SQL representations
//! ([`super::types::ResolvedFilter`]).

use anyhow::{Result, anyhow, bail};

use crate::core::{
    BLOCK_TYPE_KEY, FieldChildren, FieldDefinition, FieldType, field_children, find_field,
    flatten_array_sub_fields,
};
use crate::db::query::filter::{elements::ListLeaf, invalid_query};
use crate::db::query::helpers::{join_table, qualified_ident};
use crate::db::query::{
    column_read_expr, is_valid_identifier, join::join_rows_locale, qualified_column_read_expr,
};
use crate::db::{DbConnection, LocaleContext};

use super::json_walk::JsonWalk;
use super::lookup::{lookup_column_field, system_column_type};
use super::types::{ResolvedFilter, RowsLocale, SubqueryCondition};

/// Resolve a dot-notation filter field to its SQL representation.
///
/// Non-dot fields return [`ResolvedFilter::Column`] carrying the column's read
/// expression — the fallback `COALESCE` for a localized column, so the filter
/// compares what the SELECT returns. Dot fields are routed
/// based on the root field type:
/// - **Array** → subquery with typed column on join table; below a group,
///   nested array or nested blocks sub-field, `json_extract` (and `json_each`
///   for nesting) through the shared row walker ([`JsonWalk`]), at any depth
/// - **Blocks** → subquery with `json_extract` (and `json_each` for nesting)
///   through the same walker
/// - `{array or blocks}.id` → the row's own id column
/// - **Relationship / Upload** (has-many) → subquery on the junction's `related_id`
///
/// `locale_ctx` drives per-locale filtering on junction tables: when the
/// target array/blocks/relationship field is `localized`, the returned
/// [`ResolvedFilter::Subquery`] carries the locale its rows are read in, with
/// its fallback ([`join_rows_locale`]), so the EXISTS subquery matches exactly
/// the rows hydration shows — the fallback locale's for a document holding no
/// row in the reading locale, the default locale's for an all-locales read.
///
/// # Errors
///
/// A path the collection does not have — an unknown field or sub-field, a
/// sub-path into a field that has none — is a typed validation error naming
/// `field` ([`invalid_query`]): the caller's mistake, rejected before any SQL
/// runs, on every surface alike.
pub(in crate::db::query::filter) fn resolve_filter(
    conn: &dyn DbConnection,
    field: &str,
    slug: &str,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
) -> Result<ResolvedFilter> {
    let path = FilterPath {
        conn,
        field,
        slug,
        fields,
        locale_ctx,
    };

    resolve_path(&path).map_err(|e| invalid_query(field, format!("{e:#}")))
}

/// A filter path to resolve against the collection `slug`'s `fields`.
struct FilterPath<'a> {
    conn: &'a dyn DbConnection,
    field: &'a str,
    slug: &'a str,
    fields: &'a [FieldDefinition],
    locale_ctx: Option<&'a LocaleContext>,
}

/// [`resolve_filter`] before its errors are typed.
fn resolve_path(path: &FilterPath<'_>) -> Result<ResolvedFilter> {
    let &FilterPath {
        conn,
        field,
        slug,
        fields,
        locale_ctx,
    } = path;

    let Some((root, rest)) = field.split_once('.') else {
        return resolve_column(field, slug, fields, locale_ctx);
    };

    let field_def = find_field(root, fields)
        .ok_or_else(|| anyhow!("Unknown field '{root}' in filter path '{field}'"))?;

    // The junction table has a `_locale` column iff the container field is
    // itself localized. Transparent layout wrappers (Row/Collapsible/Tabs)
    // do not carry localization, so we only need to check `field_def.localized`.
    // Nested containers inside a localized Group use `{group}__{array}` dot
    // notation that does not route here (the root must be a top-level
    // Array/Blocks/Relationship), so inherited Group locale does not apply at
    // this call site.
    let rows_locale = join_rows_locale(field_def, locale_ctx)
        .map(|read| RowsLocale::new(read.locale, read.fallback));

    let ctx = SubFilterCtx {
        conn,
        root,
        rest,
        field,
        slug,
        field_def,
        join_table: join_table(slug, root),
        rows_locale,
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

    let field_type = leaf.map_or_else(|| system_column_type(field), |f| Some(f.field_type.clone()));

    Ok(ResolvedFilter::Column {
        expr,
        field_type,
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
    rows_locale: Option<RowsLocale>,
}

/// The join-table column every array and blocks row carries beside its
/// values: the row's own id, which a write round-trips. Only a top-level row
/// has one — a row nested in another row's JSON is not addressed by id.
pub(in crate::db::query::filter) const ROW_ID: &str = "id";

fn resolve_array_filter(ctx: SubFilterCtx<'_>) -> Result<ResolvedFilter> {
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
    let (each_joins, extract_expr, field_type, list) =
        JsonWalk::array_column(ctx.conn, column, container)?.walk(ctx.conn, &segments)?;

    Ok(ResolvedFilter::Subquery {
        join_table: ctx.join_table,
        parent_table: ctx.slug.to_string(),
        condition: SubqueryCondition::Json {
            each_joins,
            extract_expr,
            field_type,
            list,
        },
        rows_locale: ctx.rows_locale,
    })
}

/// A plain array sub-field — a typed column on the join table. A name the
/// rows do not have is rejected here, before it reaches SQL as a column, and
/// so is a container (a group, a nested array or blocks), whose column holds
/// JSON rather than a value.
fn resolve_array_column(ctx: SubFilterCtx<'_>) -> Result<ResolvedFilter> {
    let leaf = array_sub_field(ctx.field_def, ctx.rest);

    match leaf.map(field_children) {
        None if ctx.rest != ROW_ID => {
            bail!("Unknown sub-field '{}' in array '{}'", ctx.rest, ctx.root);
        }
        Some(children) if !matches!(children, FieldChildren::Leaf) => {
            bail!(
                "Filter path must end on a value field, not the container '{}'",
                ctx.rest
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

fn resolve_blocks_filter(ctx: SubFilterCtx<'_>) -> Result<ResolvedFilter> {
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
    let (each_joins, extract_expr, field_type, list) =
        JsonWalk::block_row(&ctx.join_table, &ctx.field_def.blocks).walk(ctx.conn, &rest_parts)?;

    Ok(ResolvedFilter::Subquery {
        join_table: ctx.join_table,
        parent_table: ctx.slug.to_string(),
        condition: SubqueryCondition::Json {
            each_joins,
            extract_expr,
            field_type,
            list,
        },
        rows_locale: ctx.rows_locale,
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
    use crate::config::LocaleConfig;
    use crate::core::{RelationshipConfig, ValidationError};
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

    /// Regression: the system timestamps resolved with no type and compared
    /// as plain text, so a bare-day operand never covered its day. They now
    /// resolve as dates.
    #[test]
    fn resolve_filter_system_timestamps_are_dates() {
        let (_dir, conn) = test_conn();

        for column in ["created_at", "updated_at"] {
            let resolved = resolve_filter(&conn, column, "posts", &[], None).unwrap();
            let ResolvedFilter::Column { field_type, .. } = resolved else {
                panic!("Expected Column, got {resolved:?}");
            };

            assert_eq!(field_type, Some(FieldType::Date), "{column}");
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

    fn localized_tags_rows_locale(mode: LocaleMode, fallback: bool) -> Option<RowsLocale> {
        let (_dir, conn) = test_conn();
        let mut tags = make_has_many_field("tags", "tags");
        tags.localized = true;

        let ctx = LocaleContext {
            mode,
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback,
            },
        };

        let resolved = resolve_filter(&conn, "tags.id", "posts", &[tags], Some(&ctx)).unwrap();

        match resolved {
            ResolvedFilter::Subquery { rows_locale, .. } => rows_locale,
            other => panic!("Expected Subquery, got {other:?}"),
        }
    }

    /// A localized junction's rows are matched in the locale hydration reads
    /// them in, fallback included, so a filter matches the rows the listing
    /// shows.
    #[test]
    fn resolve_filter_localized_junction_carries_the_fallback_locale() {
        assert_eq!(
            localized_tags_rows_locale(LocaleMode::Single("de".to_string()), true),
            Some(RowsLocale::new("de", Some("en")))
        );
        assert_eq!(
            localized_tags_rows_locale(LocaleMode::Single("de".to_string()), false),
            Some(RowsLocale::new("de", None))
        );
    }

    /// An all-locales read shows the default locale's rows; its filter matches
    /// those, not rows of any locale.
    #[test]
    fn resolve_filter_localized_junction_all_locales_reads_the_default() {
        assert_eq!(
            localized_tags_rows_locale(LocaleMode::All, true),
            Some(RowsLocale::new("en", None))
        );
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
                    assert_eq!(extract_expr, "json_extract(posts_content.data, '$.body')");
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

    /// The validation error a path resolves to, and the field it names.
    fn path_error(fields: &[FieldDefinition], path: &str) -> (String, String) {
        let (_dir, conn) = test_conn();
        let err = resolve_filter(&conn, path, "posts", fields, None).unwrap_err();
        let ve = err
            .downcast_ref::<ValidationError>()
            .unwrap_or_else(|| panic!("{path}: untyped error {err:#}"));

        (ve.errors[0].field.clone(), ve.errors[0].message.clone())
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

    /// Every path error is typed and names the filter path: an unknown root,
    /// a sub-path into a field without one, an unknown block field.
    #[test]
    fn resolve_filter_path_errors_are_typed() {
        let mut group = make_field("address", FieldType::Group, false);
        group.fields = vec![make_field("city", FieldType::Text, false)];
        let fields = vec![
            make_field("title", FieldType::Text, false),
            make_array_field("items", vec![group]),
            make_blocks_field(
                "content",
                vec![make_block_def(
                    "text",
                    vec![make_field("body", FieldType::Text, false)],
                )],
            ),
        ];

        for path in [
            "nope.sub",
            "title.sub",
            "items.address.nope",
            "items.address",
            "content.nope",
        ] {
            let (field, _) = path_error(&fields, path);

            assert_eq!(field, path);
        }
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

            match resolved {
                ResolvedFilter::Subquery {
                    condition:
                        SubqueryCondition::Json {
                            each_joins,
                            extract_expr,
                            field_type,
                            ..
                        },
                    ..
                } => {
                    assert_eq!(each_joins.len(), 1, "{path}");
                    assert_eq!(extract_expr, expected_expr, "{path}");
                    assert_eq!(field_type, Some(expected_type), "{path}");
                }
                other => panic!("{path}: expected Json subquery, got {other:?}"),
            }
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

        match resolved {
            ResolvedFilter::Subquery {
                condition: SubqueryCondition::Json { field_type, .. },
                ..
            } => assert_eq!(field_type, Some(FieldType::Number)),
            other => panic!("Expected Json subquery, got {other:?}"),
        }

        let (_, message) = path_error(&fields, "items.address.geo");
        assert!(message.contains("not the container 'geo'"), "{message}");
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
