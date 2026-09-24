//! WHERE clause building: each filter leaf rendered as a column condition,
//! a list condition or a join-table subquery ([`super::subquery`]).
//!
//! Locale resolution is NOT done here: every filter leaf gets its column
//! expression from [`resolve_filter`], which reads a localized column through
//! the same fallback expression the SELECT, the sort and the keyset use.

use anyhow::Result;

use super::{
    elements::{ListExpr, build_list_condition},
    operators::build_op_condition,
    resolve::{ResolvedFilter, resolve_filter},
    subquery::{SubqueryScope, build_subquery_sql},
};
use crate::core::FieldDefinition;
use crate::db::{DbConnection, DbValue, Filter, FilterClause, LocaleContext};

// ── Filter leaves ────────────────────────────────────────────────────────

/// Build a complete SQL condition for a single filter, dispatching between
/// direct column conditions and EXISTS subqueries. A scalar has-many column
/// quantifies over the elements of its list.
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
        ResolvedFilter::Column {
            expr,
            list: Some(leaf),
            ..
        } => build_list_condition(conn, f, &ListExpr::new(&expr, &leaf), params),
        ResolvedFilter::Column {
            expr, field_type, ..
        } => build_op_condition(conn, &f.field, &expr, &f.op, field_type.as_ref(), params),
        ResolvedFilter::Subquery {
            ref join_table,
            ref parent_table,
            ref condition,
            ref rows_locale,
        } => {
            let scope = SubqueryScope {
                join_table,
                parent_table,
                rows_locale: rows_locale.as_ref(),
            };

            build_subquery_sql(conn, &scope, condition, f, params)
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
///
/// # Errors
///
/// Returns an error if any filter references an unknown field or path.
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

    let mut conditions = Vec::with_capacity(filters.len());
    for clause in filters {
        conditions.push(build_clause_sql(
            conn, clause, slug, fields, locale_ctx, params,
        )?);
    }

    Ok(format!(" WHERE {}", conditions.join(" AND ")))
}

/// Render one [`FilterClause`] tree node to SQL, recursing through `And`/`Or`.
///
/// Multi-child junctions are parenthesized so nesting composes correctly; a
/// single child renders bare. An empty `And` is `1=1` (matches all), an empty
/// `Or` is `1=0` (matches none).
fn build_clause_sql(
    conn: &dyn DbConnection,
    clause: &FilterClause,
    slug: &str,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    match clause {
        FilterClause::Single(f) => build_filter_sql(conn, f, slug, fields, locale_ctx, params),
        FilterClause::And(subs) => {
            build_junction_sql(conn, subs, "AND", "1=1", slug, fields, locale_ctx, params)
        }
        FilterClause::Or(subs) => {
            build_junction_sql(conn, subs, "OR", "1=0", slug, fields, locale_ctx, params)
        }
    }
}

/// Render a conjunction/disjunction of sub-clauses joined by `sep`, using
/// `empty` as the identity when there are none. A single sub-clause renders
/// without wrapping parens.
#[allow(clippy::too_many_arguments)]
fn build_junction_sql(
    conn: &dyn DbConnection,
    subs: &[FilterClause],
    sep: &str,
    empty: &str,
    slug: &str,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    if subs.is_empty() {
        return Ok(empty.to_string());
    }

    let mut parts: Vec<String> = subs
        .iter()
        .map(|c| build_clause_sql(conn, c, slug, fields, locale_ctx, params))
        .collect::<Result<_>>()?;

    if parts.len() == 1 {
        return Ok(parts.swap_remove(0));
    }

    Ok(format!("({})", parts.join(&format!(" {sep} "))))
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
    use tempfile::TempDir;

    use super::*;
    use crate::{
        config::{CrapConfig, LocaleConfig},
        core::{
            BlockDefinition, CollectionDefinition, FieldDefinition, FieldTab, FieldType,
            RelationshipConfig,
        },
        db::{
            BoxedConnection, DbValue, pool,
            query::{Filter, FilterClause, FilterOp, LocaleContext, LocaleMode, column_read_expr},
        },
    };

    fn test_conn() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
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
        assert_eq!(sql, " WHERE \"status\" = ?1");
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
        assert_eq!(sql, " WHERE \"status\" = ?1 AND \"role\" = ?2");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn where_clause_or_groups() {
        let (_dir, conn) = test_conn();
        let filters = vec![FilterClause::or_groups(vec![
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
        assert_eq!(sql, " WHERE (\"a\" = ?1 OR (\"b\" = ?2 AND \"c\" = ?3))");
        assert_eq!(params.len(), 3);
    }

    #[test]
    fn where_clause_or_single_item_group() {
        let (_dir, conn) = test_conn();
        let filters = vec![FilterClause::or_groups(vec![vec![Filter {
            field: "a".into(),
            op: FilterOp::Equals("1".into()),
        }]])];
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_where_clause(&conn, &filters, "test", &[], None, &mut params).unwrap();
        // Single-item OR should simplify to just the condition
        assert_eq!(sql, " WHERE \"a\" = ?1");
    }

    /// The recursive tree expresses nesting the old flat OR-of-AND-groups could
    /// not: an `Or` whose arms are `And`s, one of which nests another `Or`.
    /// This is exactly the shape the access-view union produces, so the builder
    /// must parenthesize every level correctly.
    #[test]
    fn where_clause_nested_and_or_compose() {
        let (_dir, conn) = test_conn();
        let single = |f: &str, v: &str| {
            FilterClause::Single(Filter {
                field: f.into(),
                op: FilterOp::Equals(v.into()),
            })
        };
        let filters = vec![FilterClause::Or(vec![
            FilterClause::And(vec![single("a", "1"), single("b", "2")]),
            FilterClause::And(vec![
                single("c", "3"),
                FilterClause::Or(vec![single("d", "4"), single("e", "5")]),
            ]),
        ])];
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_where_clause(&conn, &filters, "test", &[], None, &mut params).unwrap();
        assert_eq!(
            sql,
            " WHERE ((\"a\" = ?1 AND \"b\" = ?2) OR (\"c\" = ?3 AND (\"d\" = ?4 OR \"e\" = ?5)))"
        );
        assert_eq!(params.len(), 5);
    }

    /// Empty junctions render their boolean identity: an empty `And` matches all
    /// rows (`1=1`), an empty `Or` matches none (`1=0`). A denied view branch in
    /// the union relies on this.
    #[test]
    fn where_clause_empty_junction_identities() {
        let (_dir, conn) = test_conn();

        let mut p1: Vec<DbValue> = Vec::new();
        let and_empty = build_where_clause(
            &conn,
            &[FilterClause::And(vec![])],
            "test",
            &[],
            None,
            &mut p1,
        )
        .unwrap();
        assert_eq!(and_empty, " WHERE 1=1");

        let mut p2: Vec<DbValue> = Vec::new();
        let or_empty = build_where_clause(
            &conn,
            &[FilterClause::Or(vec![])],
            "test",
            &[],
            None,
            &mut p2,
        )
        .unwrap();
        assert_eq!(or_empty, " WHERE 1=0");
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
            " WHERE \"status\" = ?1 AND EXISTS (SELECT 1 FROM \"posts_items\" WHERE parent_id = \"posts\".id AND \"name\" = ?2)"
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
        let filters = vec![FilterClause::or_groups(vec![
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
            " WHERE (\"status\" = ?1 OR EXISTS (SELECT 1 FROM \"posts_tags\" WHERE parent_id = \"posts\".id AND \"related_id\" = ?2))"
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
            "EXISTS (SELECT 1 FROM \"posts_items\" WHERE parent_id = \"posts\".id AND \"name\" = ?1)"
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
            "EXISTS (SELECT 1 FROM \"posts_content\" WHERE \"posts_content\".parent_id = \"posts\".id AND json_extract(posts_content.data, '$.body') LIKE ?1 ESCAPE '\\')"
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
            "EXISTS (SELECT 1 FROM \"posts_tags\" WHERE parent_id = \"posts\".id AND \"related_id\" = ?1)"
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
            "EXISTS (SELECT 1 FROM \"posts_tags\" WHERE parent_id = \"posts\".id AND \"related_id\" IN (?1, ?2))"
        );
        assert_eq!(params.len(), 2);
    }

    // ── column_read_expr (the locale read expression) ─────────────────────

    /// The `de` read expression of a localized column under a configuration
    /// with fallback on.
    fn de_expr(column: &str) -> String {
        format!("COALESCE(\"{column}__de\", \"{column}__en\")")
    }

    fn de_ctx() -> LocaleContext {
        LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: locale_config_en_de(),
        }
    }

    fn read_expr(name: &str, def: &CollectionDefinition, ctx: Option<&LocaleContext>) -> String {
        column_read_expr(name, &def.fields, ctx).unwrap()
    }

    #[test]
    fn read_expr_non_localized_passthrough() {
        let def = make_collection(vec![make_field("title", FieldType::Text, false)]);

        assert_eq!(read_expr("title", &def, Some(&de_ctx())), "\"title\"");
    }

    #[test]
    fn read_expr_localized_single_locale() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);

        assert_eq!(read_expr("title", &def, Some(&de_ctx())), de_expr("title"));
    }

    #[test]
    fn read_expr_group_sub_field_localized() {
        let mut group = make_field("meta", FieldType::Group, false);
        group.fields = vec![make_field("description", FieldType::Text, true)];
        let def = make_collection(vec![group]);

        assert_eq!(
            read_expr("meta__description", &def, Some(&de_ctx())),
            de_expr("meta__description")
        );
    }

    #[test]
    fn read_expr_localized_default_mode() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_config_en_de(),
        };

        assert_eq!(read_expr("title", &def, Some(&ctx)), "\"title__en\"");
    }

    #[test]
    fn read_expr_group_localized_default_mode() {
        let mut group = make_field("meta", FieldType::Group, true);
        group.fields = vec![make_field("title", FieldType::Text, false)];
        let def = make_collection(vec![group]);
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_config_en_de(),
        };

        assert_eq!(
            read_expr("meta__title", &def, Some(&ctx)),
            "\"meta__title__en\""
        );
    }

    /// Regression: the filter/sort column resolution looked one level deep, so
    /// a localized value below a nested group or a layout wrapper inside a
    /// group resolved to a bare column that doesn't exist.
    #[test]
    fn read_expr_nested_localized_paths() {
        let deep = FieldDefinition::builder("a", FieldType::Group)
            .localized(true)
            .fields(vec![
                FieldDefinition::builder("b", FieldType::Group)
                    .fields(vec![make_field("c", FieldType::Text, false)])
                    .build(),
            ])
            .build();
        let seo = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("r", FieldType::Row)
                    .fields(vec![make_field("title", FieldType::Text, true)])
                    .build(),
            ])
            .build();
        let meta = FieldDefinition::builder("layout", FieldType::Row)
            .fields(vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .localized(true)
                    .fields(vec![make_field("title", FieldType::Text, false)])
                    .build(),
            ])
            .build();
        let def = make_collection(vec![deep, seo, meta]);
        let ctx = de_ctx();

        for name in ["a__b__c", "seo__title", "meta__title"] {
            assert_eq!(read_expr(name, &def, Some(&ctx)), de_expr(name));
        }
    }

    #[test]
    fn read_expr_no_locale_ctx() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);

        assert_eq!(read_expr("title", &def, None), "\"title\"");
    }

    #[test]
    fn read_expr_row_sub_field_localized() {
        let mut row_field = make_field("layout", FieldType::Row, false);
        row_field.fields = vec![make_field("slug", FieldType::Text, true)];
        let def = make_collection(vec![row_field]);

        assert_eq!(read_expr("slug", &def, Some(&de_ctx())), de_expr("slug"));
    }

    #[test]
    fn read_expr_row_sub_field_non_localized_passthrough() {
        let mut row_field = make_field("layout", FieldType::Row, false);
        row_field.fields = vec![make_field("slug", FieldType::Text, false)];
        let def = make_collection(vec![row_field]);

        assert_eq!(read_expr("slug", &def, Some(&de_ctx())), "\"slug\"");
    }

    #[test]
    fn read_expr_collapsible_sub_field_localized() {
        let mut collapsible = make_field("advanced", FieldType::Collapsible, false);
        collapsible.fields = vec![make_field("summary", FieldType::Textarea, true)];
        let def = make_collection(vec![collapsible]);

        assert_eq!(
            read_expr("summary", &def, Some(&de_ctx())),
            de_expr("summary")
        );
    }

    #[test]
    fn read_expr_tabs_sub_field_localized() {
        let tabs_field = FieldDefinition::builder("page_tabs", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "Content",
                vec![make_field("description", FieldType::Textarea, true)],
            )])
            .build();
        let def = make_collection(vec![tabs_field]);

        assert_eq!(
            read_expr("description", &def, Some(&de_ctx())),
            de_expr("description")
        );
    }

    #[test]
    fn read_expr_tabs_sub_field_non_localized_passthrough() {
        let tabs_field = FieldDefinition::builder("page_tabs", FieldType::Tabs)
            .tabs(vec![FieldTab::new(
                "Content",
                vec![make_field("description", FieldType::Textarea, false)],
            )])
            .build();
        let def = make_collection(vec![tabs_field]);

        let ctx = de_ctx();

        assert_eq!(
            read_expr("description", &def, Some(&ctx)),
            "\"description\""
        );
    }

    #[test]
    fn read_expr_locale_disabled_passthrough() {
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".into()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec![], // empty = disabled
                fallback: false,
            },
        };

        assert_eq!(read_expr("title", &def, Some(&ctx)), "\"title\"");
    }

    // ── locale in the WHERE clause ────────────────────────────────────────

    #[test]
    fn where_clause_non_localized_passthrough() {
        let (_dir, conn) = test_conn();
        let def = make_collection(vec![make_field("status", FieldType::Text, false)]);
        let filters = vec![FilterClause::Single(Filter {
            field: "status".into(),
            op: FilterOp::Equals("active".into()),
        })];

        let mut params = Vec::new();
        let sql = build_where_clause(
            &conn,
            &filters,
            "test",
            &def.fields,
            Some(&de_ctx()),
            &mut params,
        )
        .unwrap();

        assert_eq!(sql, " WHERE \"status\" = ?1");
    }

    #[test]
    fn where_clause_applies_the_locale_read_expression_in_or_groups() {
        let (_dir, conn) = test_conn();
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let filters = vec![FilterClause::or_groups(vec![
            vec![Filter {
                field: "title".into(),
                op: FilterOp::Equals("A".into()),
            }],
            vec![Filter {
                field: "title".into(),
                op: FilterOp::Equals("B".into()),
            }],
        ])];

        let mut params = Vec::new();
        let sql = build_where_clause(
            &conn,
            &filters,
            "test",
            &def.fields,
            Some(&de_ctx()),
            &mut params,
        )
        .unwrap();

        let expr = de_expr("title");
        assert_eq!(sql, format!(" WHERE ({expr} = ?1 OR {expr} = ?2)"));
    }

    /// A localized filter compares the same expression the SELECT returns the
    /// value through, so a row listed under its fallback value still matches.
    #[test]
    fn where_clause_filters_a_localized_column_through_its_fallback() {
        let (_dir, conn) = test_conn();
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let filters = vec![FilterClause::Single(Filter {
            field: "title".into(),
            op: FilterOp::Equals("Hallo".into()),
        })];

        let mut params = Vec::new();
        let sql = build_where_clause(
            &conn,
            &filters,
            "test",
            &def.fields,
            Some(&de_ctx()),
            &mut params,
        )
        .unwrap();

        assert_eq!(sql, format!(" WHERE {} = ?1", de_expr("title")));
    }

    /// A locale code with capitals (`de-DE` → `title__de_DE`) names a column
    /// Postgres folds to lowercase unless it is quoted; the condition quotes it.
    #[test]
    fn uppercase_locale_column_is_quoted_in_the_where_clause() {
        let (_dir, conn) = test_conn();
        let def = make_collection(vec![make_field("title", FieldType::Text, true)]);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de-DE".into()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de-DE".to_string()],
                fallback: false,
            },
        };
        let filters = vec![FilterClause::Single(Filter {
            field: "title".into(),
            op: FilterOp::Equals("Hallo".into()),
        })];

        let mut params = Vec::new();
        let sql = build_where_clause(
            &conn,
            &filters,
            "test",
            &def.fields,
            Some(&ctx),
            &mut params,
        )
        .unwrap();

        assert_eq!(sql, " WHERE \"title__de_DE\" = ?1");
    }
}
