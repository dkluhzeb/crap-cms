//! Resolve dot-notation filter paths to SQL representations
//! ([`super::types::ResolvedFilter`]).

use anyhow::{Result, anyhow, bail};

use crate::core::{FieldDefinition, FieldType, find_field};
use crate::db::query::helpers::join_table;
use crate::db::query::is_valid_identifier;
use crate::db::{DbConnection, LocaleContext, LocaleMode};

use super::blocks::walk_block_fields;
use super::lookup::lookup_column_field_type;
use super::types::{ResolvedFilter, SubqueryCondition};

/// Resolve a dot-notation filter field to its SQL representation.
///
/// Non-dot fields return [`ResolvedFilter::Column`]. Dot fields are routed
/// based on the root field type:
/// - **Array** → subquery with typed column on join table
/// - **Blocks** → subquery with `json_extract` (and `json_each` for nesting)
/// - **Relationship** (has-many) → subquery on `related_id`
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
        let field_type = lookup_column_field_type(field, fields);
        return Ok(ResolvedFilter::Column {
            col: field.to_string(),
            field_type,
        });
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
        FieldType::Relationship => resolve_relationship_filter(ctx),
        _ => bail!(
            "Field '{}' (type {:?}) does not support sub-field filtering",
            root,
            field_def.field_type
        ),
    }
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

/// Pick the locale string to use as a `_locale = ?` constraint for a subquery.
///
/// `Single(loc)` → use that locale. `Default` → use the configured default.
/// `All` → `None` (match across all locales).
fn subquery_locale(ctx: &LocaleContext) -> Option<String> {
    match &ctx.mode {
        LocaleMode::Single(l) => Some(l.clone()),
        LocaleMode::Default => Some(ctx.config.default_locale.clone()),
        LocaleMode::All => None,
    }
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
        let sub_def = ctx.field_def.fields.iter().find(|f| f.name == first_seg);
        match sub_def.map(|f| &f.field_type) {
            Some(FieldType::Group) => {
                // Group sub-fields in arrays are stored as JSON TEXT columns.
                // Access nested values via json_extract.
                let extract_expr = ctx.conn.json_extract_expr(first_seg, remaining);
                let field_type = sub_def
                    .and_then(|g| find_field(remaining, &g.fields))
                    .map(|f| f.field_type.clone());
                Ok(ResolvedFilter::Subquery {
                    join_table: ctx.join_table,
                    parent_table: ctx.slug.to_string(),
                    condition: SubqueryCondition::Json {
                        each_joins: vec![],
                        extract_expr,
                        field_type,
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
        let field_type = ctx
            .field_def
            .fields
            .iter()
            .find(|f| f.name == ctx.rest)
            .map(|f| f.field_type.clone());
        Ok(ResolvedFilter::Subquery {
            join_table: ctx.join_table,
            parent_table: ctx.slug.to_string(),
            condition: SubqueryCondition::Column {
                col: ctx.rest.to_string(),
                field_type,
            },
            locale_constraint: ctx.locale_constraint,
        })
    }
}

fn resolve_blocks_filter(ctx: SubFilterCtx<'_>) -> Result<ResolvedFilter> {
    if ctx.rest == "_block_type" {
        return Ok(ResolvedFilter::Subquery {
            join_table: ctx.join_table,
            parent_table: ctx.slug.to_string(),
            condition: SubqueryCondition::BlockType,
            locale_constraint: ctx.locale_constraint,
        });
    }

    let rest_parts: Vec<&str> = ctx.rest.split('.').collect();
    for seg in &rest_parts {
        if !is_valid_identifier(seg) && *seg != "_block_type" {
            bail!("Invalid segment '{}' in filter path '{}'", seg, ctx.field);
        }
    }
    let (each_joins, extract_expr, field_type) = walk_block_fields(
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
        condition: SubqueryCondition::Column {
            col: "related_id".to_string(),
            field_type: Some(FieldType::Text),
        },
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
    use crate::core::RelationshipConfig;
    use crate::db::query::filter::resolve::test_helpers::*;

    #[test]
    fn resolve_filter_no_dots_returns_column() {
        let (_dir, conn) = test_conn();
        let resolved = resolve_filter(&conn, "status", "posts", &[], None).unwrap();
        match resolved {
            ResolvedFilter::Column { col, field_type } => {
                assert_eq!(col, "status");
                assert_eq!(field_type, None);
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
                    SubqueryCondition::Column { col, field_type } => {
                        assert_eq!(col, "name");
                        assert_eq!(field_type, Some(FieldType::Text));
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
                } => {
                    assert!(each_joins.is_empty());
                    assert_eq!(extract_expr, "json_extract(address, '$.city')");
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
                match condition {
                    SubqueryCondition::Column { col, field_type } => {
                        assert_eq!(col, "related_id");
                        assert_eq!(field_type, Some(FieldType::Text));
                    }
                    other => panic!("Expected Column, got {other:?}"),
                }
            }
            other => panic!("Expected Subquery, got {other:?}"),
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
