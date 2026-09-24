//! Resolve dot-notation filter paths to SQL representations
//! ([`super::types::ResolvedFilter`]).

use anyhow::{Error, Result, anyhow, bail};

use crate::core::{FieldDefinition, FieldType, find_field};
use crate::db::query::filter::{elements::ListLeaf, invalid_query};
use crate::db::query::helpers::join_table;
use crate::db::query::{column_read_expr, join::join_rows_locale, qualified_column_read_expr};
use crate::db::{DbConnection, LocaleContext};

use super::container::container_root;
use super::lookup::{lookup_column_field, system_column_type};
use super::rows::{resolve_array_filter, resolve_blocks_filter};
use super::types::{ResolvedFilter, RowsLocale, SubqueryCondition};

/// Resolve a dot-notation filter field to its SQL representation.
///
/// Non-dot fields return [`ResolvedFilter::Column`] carrying the column's read
/// expression — the fallback `COALESCE` for a localized column, so the filter
/// compares what the SELECT returns. Dot fields start at a join-table field —
/// at the top level or inside groups, the group part spelled `seo.items` or
/// `seo__items` (its table is `{slug}_seo__items`) — and are routed by its
/// type:
/// - **Array** → subquery with typed column on join table; below a group,
///   nested array or nested blocks sub-field, `json_extract` (and `json_each`
///   for nesting) through the shared row walker
///   ([`JsonWalk`](super::json_walk::JsonWalk)), at any depth
/// - **Blocks** → subquery with `json_extract` (and `json_each` for nesting)
///   through the same walker — one reading per block type where block types
///   define the path differently
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

    let Some((root, _)) = field.split_once('.') else {
        return resolve_column(field, slug, fields, locale_ctx);
    };

    let Some(container) = container_root(field, fields) else {
        return Err(no_container_error(root, field, fields));
    };

    // The join table has a `_locale` column iff the container field is itself
    // localized — hydration reads its rows by the same rule, whether the field
    // is top-level or inside groups.
    let rows_locale = join_rows_locale(container.field, locale_ctx)
        .map(|read| RowsLocale::new(read.locale, read.fallback));

    let ctx = SubFilterCtx {
        conn,
        root: container.written,
        rest: container.rest,
        field,
        slug,
        field_def: container.field,
        join_table: join_table(slug, &container.name),
        rows_locale,
    };

    match container.field.field_type {
        FieldType::Array => resolve_array_filter(ctx),
        FieldType::Blocks => resolve_blocks_filter(ctx),
        _ => resolve_relationship_filter(ctx),
    }
}

/// Why a dotted path that starts at no array, blocks or has-many field has
/// no sub-field to filter.
fn no_container_error(root: &str, field: &str, fields: &[FieldDefinition]) -> Error {
    let Some(root_def) = find_field(root, fields) else {
        return anyhow!("Unknown field '{root}' in filter path '{field}'");
    };

    match root_def.field_type {
        FieldType::Group => anyhow!(
            "Filter path '{field}' must name a value of group '{root}' or reach an array, \
             blocks or has-many field inside it"
        ),
        FieldType::Relationship | FieldType::Upload if root_def.relationship.is_none() => {
            anyhow!("Relationship field '{root}' missing relationship config")
        }
        FieldType::Relationship | FieldType::Upload => {
            anyhow!("Has-one relationship '{root}' does not use dot notation for filtering")
        }
        _ => anyhow!(
            "Field '{}' (type {:?}) does not support sub-field filtering",
            root,
            root_def.field_type
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
pub(super) struct SubFilterCtx<'a> {
    pub(super) conn: &'a dyn DbConnection,
    /// The join-table field's path, as written.
    pub(super) root: &'a str,
    /// The path below the join-table field.
    pub(super) rest: &'a str,
    /// The whole filter path.
    pub(super) field: &'a str,
    pub(super) slug: &'a str,
    pub(super) field_def: &'a FieldDefinition,
    pub(super) join_table: String,
    pub(super) rows_locale: Option<RowsLocale>,
}

/// A has-many relationship or upload: its junction rows are its elements,
/// filtered by the id each holds.
fn resolve_relationship_filter(ctx: SubFilterCtx<'_>) -> Result<ResolvedFilter> {
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

    /// A group holding an array, blocks and a has-many relationship.
    fn seo_group() -> FieldDefinition {
        let mut seo = make_field("seo", FieldType::Group, false);
        seo.fields = vec![
            make_field("title", FieldType::Text, false),
            make_array_field("links", vec![make_field("url", FieldType::Text, false)]),
            make_blocks_field(
                "sections",
                vec![make_block_def(
                    "text",
                    vec![make_field("body", FieldType::Text, false)],
                )],
            ),
            make_has_many_field("tags", "tags"),
        ];

        seo
    }

    /// Regression: an array, blocks or has-many field inside a group could not
    /// be filtered — the path was rewritten to one flat column name. It
    /// reaches the field's own join table, `{collection}_{group}__{field}`,
    /// with the group part spelled either way.
    #[test]
    fn resolve_filter_reaches_join_fields_inside_groups() {
        let (_dir, conn) = test_conn();
        let fields = vec![seo_group()];

        for (path, table) in [
            ("seo.links.url", "posts_seo__links"),
            ("seo__links.url", "posts_seo__links"),
            ("seo.links.id", "posts_seo__links"),
            ("seo.sections.body", "posts_seo__sections"),
            ("seo.sections._block_type", "posts_seo__sections"),
            ("seo.tags.id", "posts_seo__tags"),
        ] {
            let resolved = resolve_filter(&conn, path, "posts", &fields, None).unwrap();

            let ResolvedFilter::Subquery { join_table, .. } = resolved else {
                panic!("{path}: expected a subquery, got {resolved:?}");
            };

            assert_eq!(join_table, table, "{path}");
        }

        let resolved = resolve_filter(&conn, "seo.tags.id", "posts", &fields, None).unwrap();
        assert!(matches!(
            resolved,
            ResolvedFilter::Subquery {
                condition: SubqueryCondition::RelatedId,
                ..
            }
        ));
    }

    /// A path into a group's join field names itself as written in its error.
    #[test]
    fn resolve_filter_group_join_field_errors_name_the_path_as_written() {
        let fields = vec![seo_group()];

        let (field, message) = path_error(&fields, "seo.links.nope");
        assert_eq!(field, "seo.links.nope");
        assert!(
            message.contains("Unknown sub-field 'nope' in array 'seo.links'"),
            "{message}"
        );

        let (field, message) = path_error(&fields, "seo.tags.name");
        assert_eq!(field, "seo.tags.name");
        assert!(
            message.contains("'seo.tags' can only be filtered by '.id'"),
            "{message}"
        );

        let (_, message) = path_error(&fields, "seo.nope.x");
        assert!(message.contains("group 'seo'"), "{message}");
    }

    /// A localized join field inside a group reads its rows in the reading
    /// locale, as hydration does.
    #[test]
    fn resolve_filter_localized_join_field_inside_a_group_carries_its_locale() {
        let (_dir, conn) = test_conn();
        let mut seo = seo_group();
        seo.fields[1].localized = true;
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: false,
            },
        };

        let resolved = resolve_filter(&conn, "seo.links.url", "posts", &[seo], Some(&ctx)).unwrap();

        let ResolvedFilter::Subquery { rows_locale, .. } = resolved else {
            panic!("Expected a subquery, got {resolved:?}");
        };
        assert_eq!(rows_locale, Some(RowsLocale::new("de", None)));
    }
}
