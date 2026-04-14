//! WHERE clause building, subquery SQL generation, and locale column resolution.

use anyhow::{Result, bail};

use super::{
    operators::{build_filter_condition, build_op_condition},
    resolve::{ResolvedFilter, SubqueryCondition, resolve_filter},
};
use crate::core::{CollectionDefinition, FieldDefinition, FieldType};
use crate::db::{
    DbConnection, DbValue, Filter, FilterClause, FilterOp, LocaleContext, LocaleMode,
    query::{helpers::locale_column, is_valid_identifier},
};

// ── Subquery SQL generation ──────────────────────────────────────────────

/// Build a complete SQL condition for a single filter, dispatching between
/// direct column conditions and EXISTS subqueries.
fn build_filter_sql(
    conn: &dyn DbConnection,
    f: &Filter,
    slug: &str,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    let resolved = resolve_filter(conn, &f.field, slug, fields, locale_ctx)?;
    match resolved {
        ResolvedFilter::Column { col, field_type } => build_filter_condition(
            conn,
            &Filter {
                field: col,
                op: f.op.clone(),
            },
            field_type.as_ref(),
            params,
        ),
        ResolvedFilter::Subquery {
            ref join_table,
            ref parent_table,
            ref condition,
            ref locale_constraint,
        } => build_subquery_sql(
            conn,
            join_table,
            parent_table,
            condition,
            locale_constraint.as_deref(),
            &f.op,
            params,
        ),
    }
}

/// Generate an `EXISTS (SELECT 1 FROM … WHERE …)` clause for a subquery filter.
///
/// When `locale_constraint` is `Some(locale)`, an extra `"{join_table}"._locale = ?`
/// clause is appended so filters on localized junction tables (arrays, blocks,
/// has-many relationships whose parent is localized) only match rows belonging
/// to the active locale.
fn build_subquery_sql(
    conn: &dyn DbConnection,
    join_table: &str,
    parent_table: &str,
    condition: &SubqueryCondition,
    locale_constraint: Option<&str>,
    op: &FilterOp,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    match condition {
        SubqueryCondition::Column { col, field_type } => {
            if !is_valid_identifier(col) {
                bail!("Invalid column name '{}' in subquery", col);
            }
            let cond = build_op_condition(conn, col, op, field_type.as_ref(), params);
            let locale_sql = append_locale_clause(conn, join_table, locale_constraint, params);
            Ok(format!(
                "EXISTS (SELECT 1 FROM \"{}\" WHERE parent_id = \"{}\".id AND {}{})",
                join_table, parent_table, cond, locale_sql
            ))
        }
        SubqueryCondition::BlockType => {
            let cond = build_op_condition(conn, "_block_type", op, Some(&FieldType::Text), params);
            let locale_sql = append_locale_clause(conn, join_table, locale_constraint, params);
            Ok(format!(
                "EXISTS (SELECT 1 FROM \"{}\" WHERE parent_id = \"{}\".id AND {}{})",
                join_table, parent_table, cond, locale_sql
            ))
        }
        SubqueryCondition::Json {
            each_joins,
            extract_expr,
            field_type,
        } => {
            let mut from_parts = vec![format!("\"{}\"", join_table)];
            for (source, alias) in each_joins {
                from_parts.push(conn.json_each_source(source, alias));
            }
            let cond = build_op_condition(conn, extract_expr, op, field_type.as_ref(), params);
            let locale_sql = append_locale_clause(conn, join_table, locale_constraint, params);
            Ok(format!(
                "EXISTS (SELECT 1 FROM {} WHERE \"{}\".parent_id = \"{}\".id AND {}{})",
                from_parts.join(", "),
                join_table,
                parent_table,
                cond,
                locale_sql
            ))
        }
    }
}

/// Produce the trailing `AND "{join_table}"._locale = ?` fragment and push
/// the locale bind parameter, or return `""` when no locale constraint applies.
fn append_locale_clause(
    conn: &dyn DbConnection,
    join_table: &str,
    locale_constraint: Option<&str>,
    params: &mut Vec<DbValue>,
) -> String {
    let Some(locale) = locale_constraint else {
        return String::new();
    };

    params.push(DbValue::Text(locale.to_string()));
    format!(
        " AND \"{}\"._locale = {}",
        join_table,
        conn.placeholder(params.len())
    )
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
    conn: &dyn DbConnection,
    filters: &[FilterClause],
    slug: &str,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    if filters.is_empty() {
        return Ok(String::new());
    }

    let mut conditions = Vec::new();
    for clause in filters {
        match clause {
            FilterClause::Single(f) => {
                conditions.push(build_filter_sql(conn, f, slug, fields, locale_ctx, params)?);
            }
            FilterClause::Or(groups) => {
                if groups.len() == 1 && groups[0].len() == 1 {
                    conditions.push(build_filter_sql(
                        conn,
                        &groups[0][0],
                        slug,
                        fields,
                        locale_ctx,
                        params,
                    )?);
                } else {
                    let mut or_parts = Vec::new();
                    for group in groups {
                        if group.len() == 1 {
                            or_parts.push(build_filter_sql(
                                conn, &group[0], slug, fields, locale_ctx, params,
                            )?);
                        } else {
                            let and_parts: Vec<String> = group
                                .iter()
                                .map(|f| {
                                    build_filter_sql(conn, f, slug, fields, locale_ctx, params)
                                })
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
) -> Result<Vec<FilterClause>> {
    filters
        .iter()
        .map(|clause| match clause {
            FilterClause::Single(f) => {
                let resolved = resolve_filter_column(&f.field, def, locale_ctx)?;
                Ok(FilterClause::Single(Filter {
                    field: resolved,
                    op: f.op.clone(),
                }))
            }
            FilterClause::Or(groups) => {
                let resolved_groups: Result<Vec<Vec<Filter>>> = groups
                    .iter()
                    .map(|group| {
                        group
                            .iter()
                            .map(|f| {
                                let resolved = resolve_filter_column(&f.field, def, locale_ctx)?;
                                Ok(Filter {
                                    field: resolved,
                                    op: f.op.clone(),
                                })
                            })
                            .collect()
                    })
                    .collect();

                Ok(FilterClause::Or(resolved_groups?))
            }
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
) -> Result<String> {
    if let Some(ctx) = locale_ctx
        && ctx.config.is_enabled()
    {
        for field in &def.fields {
            if let Some(locale) = check_field_locale(field, field_name, ctx) {
                return locale_column(field_name, locale);
            }
        }
    }

    Ok(field_name.to_string())
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
    let prefix = format!("{}__", field.name);

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
    use crate::db::{
        DbValue,
        query::{Filter, FilterClause, FilterOp, LocaleContext, LocaleMode},
    };

    fn test_conn() -> (tempfile::TempDir, crate::db::BoxedConnection) {
        let dir = tempfile::TempDir::new().unwrap();
        let config = crate::config::CrapConfig::default();
        let p = crate::db::pool::create_pool(dir.path(), &config).unwrap();
        (dir, p.get().unwrap())
    }

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
        let (_dir, conn) = test_conn();
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_where_clause(&conn, &[], "test", &[], None, &mut params).unwrap();
        assert_eq!(sql, "");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn where_clause_single_filter() {
        let (_dir, conn) = test_conn();
        let filters = vec![FilterClause::Single(Filter {
            field: "status".into(),
            op: FilterOp::Equals("active".into()),
        })];
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_where_clause(&conn, &filters, "test", &[], None, &mut params).unwrap();
        assert_eq!(sql, " WHERE status = ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn where_clause_multiple_and() {
        let (_dir, conn) = test_conn();
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
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_where_clause(&conn, &filters, "test", &[], None, &mut params).unwrap();
        assert_eq!(sql, " WHERE status = ?1 AND role = ?2");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn where_clause_or_groups() {
        let (_dir, conn) = test_conn();
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
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_where_clause(&conn, &filters, "test", &[], None, &mut params).unwrap();
        assert_eq!(sql, " WHERE (a = ?1 OR (b = ?2 AND c = ?3))");
        assert_eq!(params.len(), 3);
    }

    #[test]
    fn where_clause_or_single_item_group() {
        let (_dir, conn) = test_conn();
        let filters = vec![FilterClause::Or(vec![vec![Filter {
            field: "a".into(),
            op: FilterOp::Equals("1".into()),
        }]])];
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_where_clause(&conn, &filters, "test", &[], None, &mut params).unwrap();
        // Single-item OR should simplify to just the condition
        assert_eq!(sql, " WHERE a = ?1");
    }

    // ── build_where_clause with subqueries ──────────────────────────────

    #[test]
    fn where_clause_mixed_column_and_subquery() {
        let (_dir, conn) = test_conn();
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
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_where_clause(&conn, &filters, "posts", &fields, None, &mut params).unwrap();
        assert_eq!(
            sql,
            " WHERE status = ?1 AND EXISTS (SELECT 1 FROM \"posts_items\" WHERE parent_id = \"posts\".id AND name = ?2)"
        );
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn where_clause_or_with_subquery() {
        let (_dir, conn) = test_conn();
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
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_where_clause(&conn, &filters, "posts", &fields, None, &mut params).unwrap();
        assert_eq!(
            sql,
            " WHERE (status = ?1 OR EXISTS (SELECT 1 FROM \"posts_tags\" WHERE parent_id = \"posts\".id AND related_id = ?2))"
        );
        assert_eq!(params.len(), 2);
    }

    // ── build_filter_sql (subquery tests) ──────────────────────────────

    #[test]
    fn subquery_array_column() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];
        let f = Filter {
            field: "items.name".into(),
            op: FilterOp::Equals("X".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_sql(&conn, &f, "posts", &fields, None, &mut params).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM \"posts_items\" WHERE parent_id = \"posts\".id AND name = ?1)"
        );
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn subquery_block_type() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_blocks_field("content", vec![])];
        let f = Filter {
            field: "content._block_type".into(),
            op: FilterOp::Equals("image".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_sql(&conn, &f, "posts", &fields, None, &mut params).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM \"posts_content\" WHERE parent_id = \"posts\".id AND _block_type = ?1)"
        );
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn subquery_block_json_simple() {
        let (_dir, conn) = test_conn();
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
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_sql(&conn, &f, "posts", &fields, None, &mut params).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM \"posts_content\" WHERE \"posts_content\".parent_id = \"posts\".id AND json_extract(data, '$.body') LIKE ?1 ESCAPE '\\')"
        );
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn subquery_block_nested_with_json_each() {
        let (_dir, conn) = test_conn();
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
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_sql(&conn, &f, "posts", &fields, None, &mut params).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM \"posts_content\", json_each(json_extract(posts_content.data, '$.nested')) AS j0 WHERE \"posts_content\".parent_id = \"posts\".id AND json_extract(j0.value, '$.text') = ?1)"
        );
    }

    #[test]
    fn subquery_has_many_relationship() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_has_many_field("tags", "tags")];
        let f = Filter {
            field: "tags.id".into(),
            op: FilterOp::Equals("tag1".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_sql(&conn, &f, "posts", &fields, None, &mut params).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM \"posts_tags\" WHERE parent_id = \"posts\".id AND related_id = ?1)"
        );
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn subquery_with_in_operator() {
        let (_dir, conn) = test_conn();
        let fields = vec![make_has_many_field("tags", "tags")];
        let f = Filter {
            field: "tags.id".into(),
            op: FilterOp::In(vec!["a".into(), "b".into()]),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_sql(&conn, &f, "posts", &fields, None, &mut params).unwrap();
        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM \"posts_tags\" WHERE parent_id = \"posts\".id AND related_id IN (?1, ?2))"
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
        let result = resolve_filter_column("title", &def, Some(&ctx)).unwrap();
        assert_eq!(result, "title");
    }

    #[test]
    fn resolve_column_localized_single_locale() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("title", &def, Some(&ctx)).unwrap();
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
        let result = resolve_filter_column("meta__description", &def, Some(&ctx)).unwrap();
        assert_eq!(result, "meta__description__de");
    }

    #[test]
    fn resolve_column_localized_default_mode() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_config_en_de(),
        };
        let result = resolve_filter_column("title", &def, Some(&ctx)).unwrap();
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
        let result = resolve_filter_column("meta__title", &def, Some(&ctx)).unwrap();
        assert_eq!(result, "meta__title__en");
    }

    #[test]
    fn resolve_column_no_locale_ctx() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let result = resolve_filter_column("title", &def, None).unwrap();
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
        let result = resolve_filter_column("slug", &def, Some(&ctx)).unwrap();
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
        let result = resolve_filter_column("slug", &def, Some(&ctx)).unwrap();
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
        let result = resolve_filter_column("summary", &def, Some(&ctx)).unwrap();
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
        let result = resolve_filter_column("description", &def, Some(&ctx)).unwrap();
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
        let result = resolve_filter_column("description", &def, Some(&ctx)).unwrap();
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
        let result = resolve_filter_column("title", &def, Some(&ctx)).unwrap();
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
        let resolved = resolve_filters(&filters, &def, Some(&ctx)).unwrap();
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
        let resolved = resolve_filters(&filters, &def, Some(&ctx)).unwrap();
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
        let resolved = resolve_filters(&filters, &def, Some(&ctx)).unwrap();
        match &resolved[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "title__de"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }
}
