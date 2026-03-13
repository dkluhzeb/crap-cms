//! WHERE clause building, subquery SQL generation, and locale column resolution.

use anyhow::{Result, bail};

use super::{
    super::{
        Filter, FilterClause, FilterOp, LocaleContext, LocaleMode, is_valid_identifier,
        sanitize_locale,
    },
    operators::{build_filter_condition, build_op_condition},
    resolve::{ResolvedFilter, SubqueryCondition, resolve_filter},
};
use crate::core::{
    CollectionDefinition,
    field::{FieldDefinition, FieldType},
};

// ── Subquery SQL generation ──────────────────────────────────────────────

/// Build a complete SQL condition for a single filter, dispatching between
/// direct column conditions and EXISTS subqueries.
fn build_filter_sql(
    f: &Filter,
    slug: &str,
    fields: &[FieldDefinition],
    params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) -> Result<String> {
    let resolved = resolve_filter(&f.field, slug, fields)?;
    match resolved {
        ResolvedFilter::Column(col) => build_filter_condition(
            &Filter {
                field: col,
                op: f.op.clone(),
            },
            params,
        ),
        ResolvedFilter::Subquery {
            ref join_table,
            ref parent_table,
            ref condition,
        } => build_subquery_sql(join_table, parent_table, condition, &f.op, params),
    }
}

/// Generate an `EXISTS (SELECT 1 FROM … WHERE …)` clause for a subquery filter.
fn build_subquery_sql(
    join_table: &str,
    parent_table: &str,
    condition: &SubqueryCondition,
    op: &FilterOp,
    params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) -> Result<String> {
    match condition {
        SubqueryCondition::Column(col) => {
            if !is_valid_identifier(col) {
                bail!("Invalid column name '{}' in subquery", col);
            }
            let cond = build_op_condition(col, op, params);
            Ok(format!(
                "EXISTS (SELECT 1 FROM {} WHERE parent_id = {}.id AND {})",
                join_table, parent_table, cond
            ))
        }
        SubqueryCondition::BlockType => {
            let cond = build_op_condition("_block_type", op, params);
            Ok(format!(
                "EXISTS (SELECT 1 FROM {} WHERE parent_id = {}.id AND {})",
                join_table, parent_table, cond
            ))
        }
        SubqueryCondition::Json {
            each_joins,
            extract_expr,
        } => {
            let mut from_parts = vec![join_table.to_string()];
            for (source, alias) in each_joins {
                from_parts.push(format!("json_each({}) AS {}", source, alias));
            }
            let cond = build_op_condition(extract_expr, op, params);
            Ok(format!(
                "EXISTS (SELECT 1 FROM {} WHERE {}.parent_id = {}.id AND {})",
                from_parts.join(", "),
                join_table,
                parent_table,
                cond
            ))
        }
    }
}

// ── WHERE clause building ────────────────────────────────────────────────

/// Build a complete ` WHERE …` clause from a slice of [`FilterClause`]s.
///
/// Top-level clauses are joined with `AND`. An [`FilterClause::Or`] group
/// produces `(a OR b OR (c AND d))` sub-expressions, while
/// [`FilterClause::Single`] produces a plain condition.
///
/// Dot-notation fields (e.g., `items.name`, `content.body`) are resolved to
/// EXISTS subqueries against join tables. Non-dot fields use direct column
/// conditions.
///
/// Returns an **empty string** when `filters` is empty (no WHERE at all),
/// so callers can unconditionally append the result to their query.
pub fn build_where_clause(
    filters: &[FilterClause],
    slug: &str,
    fields: &[FieldDefinition],
    params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) -> Result<String> {
    if filters.is_empty() {
        return Ok(String::new());
    }

    let mut conditions = Vec::new();
    for clause in filters {
        match clause {
            FilterClause::Single(f) => {
                conditions.push(build_filter_sql(f, slug, fields, params)?);
            }
            FilterClause::Or(groups) => {
                if groups.len() == 1 && groups[0].len() == 1 {
                    conditions.push(build_filter_sql(&groups[0][0], slug, fields, params)?);
                } else {
                    let mut or_parts = Vec::new();
                    for group in groups {
                        if group.len() == 1 {
                            or_parts.push(build_filter_sql(&group[0], slug, fields, params)?);
                        } else {
                            let and_parts: Vec<String> = group
                                .iter()
                                .map(|f| build_filter_sql(f, slug, fields, params))
                                .collect::<Result<_, _>>()?;
                            or_parts.push(format!("({})", and_parts.join(" AND ")));
                        }
                    }
                    conditions.push(format!("({})", or_parts.join(" OR ")));
                }
            }
        }
    }

    Ok(format!(" WHERE {}", conditions.join(" AND ")))
}

/// Resolve filter clauses to use locale-specific column names.
///
/// Walks every [`FilterClause`] (including nested OR groups) and replaces each
/// filter's field name with the locale-suffixed column name returned by
/// [`resolve_filter_column`]. Non-localized fields pass through unchanged.
///
/// This is a pure transformation — no database access required.
pub fn resolve_filters(
    filters: &[FilterClause],
    def: &CollectionDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> Vec<FilterClause> {
    filters
        .iter()
        .map(|clause| match clause {
            FilterClause::Single(f) => {
                let resolved = resolve_filter_column(&f.field, def, locale_ctx);
                FilterClause::Single(Filter {
                    field: resolved,
                    op: f.op.clone(),
                })
            }
            FilterClause::Or(groups) => FilterClause::Or(
                groups
                    .iter()
                    .map(|group| {
                        group
                            .iter()
                            .map(|f| {
                                let resolved = resolve_filter_column(&f.field, def, locale_ctx);
                                Filter {
                                    field: resolved,
                                    op: f.op.clone(),
                                }
                            })
                            .collect()
                    })
                    .collect(),
            ),
        })
        .collect()
}

/// Map a filter field name to its actual SQL column name, accounting for locale.
///
/// When a [`LocaleContext`] is present and localization is enabled:
/// - If the field is a localized top-level field, returns `"field__locale"`.
/// - If the field is a group sub-field (`"group__sub"`) where either the group
///   or the sub-field is localized, returns `"group__sub__locale"`.
/// - For [`LocaleMode::Single`] the requested locale is used; otherwise the
///   default locale from config is used.
///
/// Non-localized fields (or disabled locale config) pass through unchanged.
pub fn resolve_filter_column(
    field_name: &str,
    def: &CollectionDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> String {
    if let Some(ctx) = locale_ctx
        && ctx.config.is_enabled()
    {
        for field in &def.fields {
            if let Some(locale) = check_field_locale(field, field_name, ctx) {
                return format!("{}__{}", field_name, sanitize_locale(locale));
            }
        }
    }
    field_name.to_string()
}

fn get_locale(ctx: &LocaleContext) -> &str {
    match &ctx.mode {
        LocaleMode::Single(l) => l.as_str(),
        _ => ctx.config.default_locale.as_str(),
    }
}

fn check_field_locale<'a>(
    field: &FieldDefinition,
    field_name: &str,
    ctx: &'a LocaleContext,
) -> Option<&'a str> {
    match field.field_type {
        FieldType::Group => check_group_locale(field, field_name, ctx),
        FieldType::Row | FieldType::Collapsible => {
            check_flat_sub_fields(&field.fields, field_name, ctx)
        }
        FieldType::Tabs => {
            for tab in &field.tabs {
                if let Some(locale) = check_flat_sub_fields(&tab.fields, field_name, ctx) {
                    return Some(locale);
                }
            }
            None
        }
        _ => {
            if field.name == field_name && field.localized {
                Some(get_locale(ctx))
            } else {
                None
            }
        }
    }
}

fn check_group_locale<'a>(
    field: &FieldDefinition,
    field_name: &str,
    ctx: &'a LocaleContext,
) -> Option<&'a str> {
    let prefix = format!("{}__{}", field.name, "");

    if field_name.starts_with(&prefix) {
        let sub_name = &field_name[prefix.len()..];
        for sub in &field.fields {
            if sub.name == sub_name && (field.localized || sub.localized) {
                return Some(get_locale(ctx));
            }
        }
    }
    None
}

fn check_flat_sub_fields<'a>(
    sub_fields: &[FieldDefinition],
    field_name: &str,
    ctx: &'a LocaleContext,
) -> Option<&'a str> {
    for sub in sub_fields {
        if sub.name == field_name && sub.localized {
            return Some(get_locale(ctx));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocaleConfig;
    use crate::core::collection::CollectionDefinition;
    use crate::core::field::{
        BlockDefinition, FieldDefinition, FieldTab, FieldType, RelationshipConfig,
    };
    use crate::db::query::{Filter, FilterClause, FilterOp, LocaleContext, LocaleMode};

    fn make_field(name: &str, ft: FieldType, localized: bool) -> FieldDefinition {
        FieldDefinition::builder(name, ft)
            .localized(localized)
            .build()
    }

    fn make_collection(fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("test");
        def.fields = fields;
        def
    }

    fn locale_config_en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    fn make_array_field(name: &str, sub_fields: Vec<FieldDefinition>) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Array)
            .fields(sub_fields)
            .build()
    }

    fn make_blocks_field(name: &str, blocks: Vec<BlockDefinition>) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Blocks)
            .blocks(blocks)
            .build()
    }

    fn make_has_many_field(name: &str, collection: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Relationship)
            .relationship(RelationshipConfig::new(collection, true))
            .build()
    }

    fn make_block_def(block_type: &str, fields: Vec<FieldDefinition>) -> BlockDefinition {
        BlockDefinition::new(block_type, fields)
    }

    // ── build_where_clause ────────────────────────────────────────────────

    #[test]
    fn where_clause_empty_filters() {
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&[], "test", &[], &mut params).unwrap();
        assert_eq!(sql, "");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn where_clause_single_filter() {
        let filters = vec![FilterClause::Single(Filter {
            field: "status".into(),
            op: FilterOp::Equals("active".into()),
        })];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, "test", &[], &mut params).unwrap();
        assert_eq!(sql, " WHERE status = ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn where_clause_multiple_and() {
        let filters = vec![
            FilterClause::Single(Filter {
                field: "status".into(),
                op: FilterOp::Equals("active".into()),
            }),
            FilterClause::Single(Filter {
                field: "role".into(),
                op: FilterOp::Equals("admin".into()),
            }),
        ];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, "test", &[], &mut params).unwrap();
        assert_eq!(sql, " WHERE status = ? AND role = ?");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn where_clause_or_groups() {
        let filters = vec![FilterClause::Or(vec![
            vec![Filter {
                field: "a".into(),
                op: FilterOp::Equals("1".into()),
            }],
            vec![
                Filter {
                    field: "b".into(),
                    op: FilterOp::Equals("2".into()),
                },
                Filter {
                    field: "c".into(),
                    op: FilterOp::Equals("3".into()),
                },
            ],
        ])];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, "test", &[], &mut params).unwrap();
        assert_eq!(sql, " WHERE (a = ? OR (b = ? AND c = ?))");
        assert_eq!(params.len(), 3);
    }

    #[test]
    fn where_clause_or_single_item_group() {
        let filters = vec![FilterClause::Or(vec![vec![Filter {
            field: "a".into(),
            op: FilterOp::Equals("1".into()),
        }]])];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, "test", &[], &mut params).unwrap();
        // Single-item OR should simplify to just the condition
        assert_eq!(sql, " WHERE a = ?");
    }

    // ── build_where_clause with subqueries ──────────────────────────────

    #[test]
    fn where_clause_mixed_column_and_subquery() {
        let fields = vec![
            make_field("status", FieldType::Text, false),
            make_array_field("items", vec![make_field("name", FieldType::Text, false)]),
        ];
        let filters = vec![
            FilterClause::Single(Filter {
                field: "status".into(),
                op: FilterOp::Equals("active".into()),
            }),
            FilterClause::Single(Filter {
                field: "items.name".into(),
                op: FilterOp::Equals("X".into()),
            }),
        ];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, "posts", &fields, &mut params).unwrap();
        assert_eq!(
            sql,
            " WHERE status = ? AND EXISTS (SELECT 1 FROM posts_items WHERE parent_id = posts.id AND name = ?)"
        );
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn where_clause_or_with_subquery() {
        let fields = vec![
            make_field("status", FieldType::Text, false),
            make_has_many_field("tags", "tags"),
        ];
        let filters = vec![FilterClause::Or(vec![
            vec![Filter {
                field: "status".into(),
                op: FilterOp::Equals("draft".into()),
            }],
            vec![Filter {
                field: "tags.id".into(),
                op: FilterOp::Equals("t1".into()),
            }],
        ])];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_where_clause(&filters, "posts", &fields, &mut params).unwrap();
        assert_eq!(
            sql,
            " WHERE (status = ? OR EXISTS (SELECT 1 FROM posts_tags WHERE parent_id = posts.id AND related_id = ?))"
        );
        assert_eq!(params.len(), 2);
    }

    // ── build_filter_sql (subquery tests) ──────────────────────────────

    #[test]
    fn subquery_array_column() {
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];
        let f = Filter {
            field: "items.name".into(),
            op: FilterOp::Equals("X".into()),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_sql(&f, "posts", &fields, &mut params).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM posts_items WHERE parent_id = posts.id AND name = ?)"
        );
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn subquery_block_type() {
        let fields = vec![make_blocks_field("content", vec![])];
        let f = Filter {
            field: "content._block_type".into(),
            op: FilterOp::Equals("image".into()),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_sql(&f, "posts", &fields, &mut params).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM posts_content WHERE parent_id = posts.id AND _block_type = ?)"
        );
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn subquery_block_json_simple() {
        let fields = vec![make_blocks_field(
            "content",
            vec![make_block_def(
                "paragraph",
                vec![make_field("body", FieldType::Textarea, false)],
            )],
        )];
        let f = Filter {
            field: "content.body".into(),
            op: FilterOp::Contains("hello".into()),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_sql(&f, "posts", &fields, &mut params).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM posts_content WHERE posts_content.parent_id = posts.id AND json_extract(data, '$.body') LIKE ? ESCAPE '\\')"
        );
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn subquery_block_nested_with_json_each() {
        let inner_blocks = vec![make_block_def(
            "quote",
            vec![make_field("text", FieldType::Text, false)],
        )];
        let mut nested = make_field("nested", FieldType::Blocks, false);
        nested.blocks = inner_blocks;
        let fields = vec![make_blocks_field(
            "content",
            vec![make_block_def("rich", vec![nested])],
        )];
        let f = Filter {
            field: "content.nested.text".into(),
            op: FilterOp::Equals("hi".into()),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_sql(&f, "posts", &fields, &mut params).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM posts_content, json_each(json_extract(posts_content.data, '$.nested')) AS j0 WHERE posts_content.parent_id = posts.id AND json_extract(j0.value, '$.text') = ?)"
        );
    }

    #[test]
    fn subquery_has_many_relationship() {
        let fields = vec![make_has_many_field("tags", "tags")];
        let f = Filter {
            field: "tags.id".into(),
            op: FilterOp::Equals("tag1".into()),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_sql(&f, "posts", &fields, &mut params).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM posts_tags WHERE parent_id = posts.id AND related_id = ?)"
        );
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn subquery_with_in_operator() {
        let fields = vec![make_has_many_field("tags", "tags")];
        let f = Filter {
            field: "tags.id".into(),
            op: FilterOp::In(vec!["a".into(), "b".into()]),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_sql(&f, "posts", &fields, &mut params).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM posts_tags WHERE parent_id = posts.id AND related_id IN (?, ?))"
        );
        assert_eq!(params.len(), 2);
    }

    // ── resolve_filter_column ─────────────────────────────────────────────

    #[test]
    fn resolve_column_non_localized_passthrough() {
        let def = make_collection(vec![make_field("title", FieldType::Text, false)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("title", &def, Some(&ctx));
        assert_eq!(result, "title");
    }

    #[test]
    fn resolve_column_localized_single_locale() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("title", &def, Some(&ctx));
        assert_eq!(result, "title__de");
    }

    #[test]
    fn resolve_column_group_sub_field_localized() {
        let mut group = make_field("meta", FieldType::Group, false);
        let sub = make_field("description", FieldType::Text, true);
        group.fields = vec![sub];

        let def = make_collection(vec![group]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("meta__description", &def, Some(&ctx));
        assert_eq!(result, "meta__description__de");
    }

    #[test]
    fn resolve_column_localized_default_mode() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("title", &def, Some(&ctx));
        assert_eq!(result, "title__en");
    }

    #[test]
    fn resolve_column_group_localized_default_mode() {
        let mut group = make_field("meta", FieldType::Group, true);
        group.fields = vec![make_field("title", FieldType::Text, false)];
        let def = make_collection(vec![group]);
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("meta__title", &def, Some(&ctx));
        assert_eq!(result, "meta__title__en");
    }

    #[test]
    fn resolve_column_no_locale_ctx() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let result = resolve_filter_column("title", &def, None);
        assert_eq!(result, "title");
    }

    #[test]
    fn resolve_column_row_sub_field_localized() {
        // Sub-field inside a Row wrapper is localized
        let mut row_field = make_field("layout", FieldType::Row, false);
        let localized_sub = make_field("slug", FieldType::Text, true);
        row_field.fields = vec![localized_sub];

        let def = make_collection(vec![row_field]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        // Filtering by "slug" should resolve to "slug__de" because it's localized inside a Row
        let result = resolve_filter_column("slug", &def, Some(&ctx));
        assert_eq!(result, "slug__de");
    }

    #[test]
    fn resolve_column_row_sub_field_non_localized_passthrough() {
        // Sub-field inside a Row wrapper is NOT localized — should pass through unchanged
        let mut row_field = make_field("layout", FieldType::Row, false);
        let non_localized_sub = make_field("slug", FieldType::Text, false);
        row_field.fields = vec![non_localized_sub];

        let def = make_collection(vec![row_field]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("slug", &def, Some(&ctx));
        assert_eq!(result, "slug");
    }

    #[test]
    fn resolve_column_collapsible_sub_field_localized() {
        // Sub-field inside a Collapsible wrapper is localized
        let mut collapsible = make_field("advanced", FieldType::Collapsible, false);
        let localized_sub = make_field("summary", FieldType::Textarea, true);
        collapsible.fields = vec![localized_sub];

        let def = make_collection(vec![collapsible]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("summary", &def, Some(&ctx));
        assert_eq!(result, "summary__de");
    }

    #[test]
    fn resolve_column_tabs_sub_field_localized() {
        // Sub-field inside a Tabs wrapper is localized
        let tabs_field = FieldDefinition::builder("page_tabs", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "Content",
                vec![make_field("description", FieldType::Textarea, true)],
            )])
            .build();
        let def = make_collection(vec![tabs_field]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("description", &def, Some(&ctx));
        assert_eq!(result, "description__de");
    }

    #[test]
    fn resolve_column_tabs_sub_field_non_localized_passthrough() {
        // Sub-field inside a Tabs wrapper is NOT localized
        let tabs_field = FieldDefinition::builder("page_tabs", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "Content",
                vec![make_field("description", FieldType::Textarea, false)],
            )])
            .build();
        let def = make_collection(vec![tabs_field]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("description", &def, Some(&ctx));
        assert_eq!(result, "description");
    }

    #[test]
    fn resolve_column_locale_disabled_passthrough() {
        // Even with a ctx, if config.is_enabled() is false, passthrough
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec![], // empty = disabled
                fallback: false,
            },
        };
        let result = resolve_filter_column("title", &def, Some(&ctx));
        assert_eq!(result, "title");
    }

    // ── resolve_filters ───────────────────────────────────────────────────

    #[test]
    fn resolve_filters_non_localized_passthrough() {
        let def = make_collection(vec![make_field("status", FieldType::Text, false)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let filters = vec![FilterClause::Single(Filter {
            field: "status".into(),
            op: FilterOp::Equals("active".into()),
        })];
        let resolved = resolve_filters(&filters, &def, Some(&ctx));
        match &resolved[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "status"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filters_or_groups() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let filters = vec![FilterClause::Or(vec![
            vec![Filter {
                field: "title".into(),
                op: FilterOp::Equals("A".into()),
            }],
            vec![Filter {
                field: "title".into(),
                op: FilterOp::Equals("B".into()),
            }],
        ])];
        let resolved = resolve_filters(&filters, &def, Some(&ctx));
        match &resolved[0] {
            FilterClause::Or(groups) => {
                assert_eq!(groups[0][0].field, "title__de");
                assert_eq!(groups[1][0].field, "title__de");
            }
            other => panic!("Expected Or, got {:?}", other),
        }
    }

    #[test]
    fn resolve_filters_applies_locale() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let filters = vec![FilterClause::Single(Filter {
            field: "title".into(),
            op: FilterOp::Equals("Hallo".into()),
        })];
        let resolved = resolve_filters(&filters, &def, Some(&ctx));
        match &resolved[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "title__de"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }
}
