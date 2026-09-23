//! EXISTS subqueries of filters on join tables — array and blocks rows and
//! has-many relationship/upload junctions.

use anyhow::{Result, bail};

use super::{
    elements::{ListExpr, ListLeaf, build_list_condition, build_quantified},
    operators::{build_filter_condition, build_op_condition},
    resolve::SubqueryCondition,
};
use crate::core::{BLOCK_TYPE_KEY, FieldType};
use crate::db::{
    DbConnection, DbValue, Filter, FilterOp,
    query::{helpers::qualified_ident, is_valid_identifier},
};

/// Where a subquery filter looks: the join table, the parent table its rows
/// belong to, and the locale its rows are constrained to.
pub(super) struct SubqueryScope<'a> {
    pub(super) join_table: &'a str,
    pub(super) parent_table: &'a str,
    pub(super) locale_constraint: Option<&'a str>,
}

/// Generate the `EXISTS (SELECT 1 FROM … WHERE …)` clause of a subquery filter.
///
/// A has-many relationship's or upload's junction rows are its elements: the
/// filter quantifies over them exactly as a scalar has-many filter quantifies
/// over its list, so `not_equals` asks that no row hold the id (`NOT EXISTS`).
/// Array and blocks rows are records — the filter asks for some row whose
/// sub-field satisfies the operator, and a list inside that row — a scalar
/// has-many list, or a has-many reference's id list — then quantifies over its
/// own elements.
pub(super) fn build_subquery_sql(
    conn: &dyn DbConnection,
    scope: &SubqueryScope<'_>,
    condition: &SubqueryCondition,
    f: &Filter,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    match condition {
        SubqueryCondition::RelatedId => build_quantified(&f.op, |op| {
            let op_sql = related_id_condition(conn, &f.field, op, params)?;

            Ok(row_select(conn, scope, None, &op_sql, params))
        }),
        SubqueryCondition::Column {
            col,
            field_type,
            list,
        } => {
            let leaf = SubqueryLeaf {
                expr: col,
                field_type: field_type.as_ref(),
                list: list.as_ref(),
            };
            let op_sql = column_condition(conn, scope, &leaf, f, params)?;

            Ok(exists_row(conn, scope, None, &op_sql, params))
        }
        SubqueryCondition::BlockType => {
            let op_sql = build_op_condition(
                conn,
                &f.field,
                BLOCK_TYPE_KEY,
                &f.op,
                Some(&FieldType::Text),
                params,
            )?;

            Ok(exists_row(conn, scope, None, &op_sql, params))
        }
        SubqueryCondition::Json {
            each_joins,
            extract_expr,
            field_type,
            list,
        } => {
            let leaf = SubqueryLeaf {
                expr: extract_expr,
                field_type: field_type.as_ref(),
                list: list.as_ref(),
            };
            let op_sql = json_condition(conn, &leaf, f, params)?;
            let from = json_from(conn, scope.join_table, each_joins);

            Ok(exists_row(conn, scope, Some(&from), &op_sql, params))
        }
    }
}

/// The value a subquery filter tests in each row: a join-table column or a
/// JSON extract, its field type, and the list it holds, if any.
struct SubqueryLeaf<'a> {
    expr: &'a str,
    field_type: Option<&'a FieldType>,
    list: Option<&'a ListLeaf>,
}

/// The per-row test of a has-many junction: the element operator on its
/// `related_id`, always text.
fn related_id_condition(
    conn: &dyn DbConnection,
    field: &str,
    op: &FilterOp,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    build_op_condition(
        conn,
        field,
        "\"related_id\"",
        op,
        Some(&FieldType::Text),
        params,
    )
}

/// The per-row test of an array sub-field column. The same validate-then-quote
/// guard the parent-table path applies, so a join-table column can't reach SQL
/// unchecked either; a list column is read qualified by its join table.
fn column_condition(
    conn: &dyn DbConnection,
    scope: &SubqueryScope<'_>,
    leaf: &SubqueryLeaf<'_>,
    f: &Filter,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    let Some(list) = leaf.list else {
        let column = Filter {
            field: leaf.expr.to_string(),
            op: f.op.clone(),
        };

        return build_filter_condition(conn, &column, &f.field, leaf.field_type, params);
    };

    if !is_valid_identifier(leaf.expr) {
        bail!(
            "Invalid field name '{}': must be alphanumeric/underscore",
            leaf.expr
        );
    }

    let list_expr = qualified_ident(Some(scope.join_table), leaf.expr);

    build_list_condition(conn, f, &ListExpr::new(&list_expr, list), params)
}

/// The per-row test of a value inside a row's JSON.
fn json_condition(
    conn: &dyn DbConnection,
    leaf: &SubqueryLeaf<'_>,
    f: &Filter,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    if let Some(list) = leaf.list {
        return build_list_condition(conn, f, &ListExpr::new(leaf.expr, list), params);
    }

    // A Number sub-field's JSON extract is text on Postgres — cast it so the
    // comparison is numeric, not lexical (or a type error).
    let extract = if matches!(leaf.field_type, Some(FieldType::Number)) {
        conn.json_number_cast(leaf.expr)
    } else {
        leaf.expr.to_string()
    };

    build_op_condition(conn, &f.field, &extract, &f.op, leaf.field_type, params)
}

/// The FROM list of a JSON subquery: the join table, then each `json_each`
/// expansion down to the filtered value.
fn json_from(conn: &dyn DbConnection, join_table: &str, each_joins: &[(String, String)]) -> String {
    let mut parts = vec![format!("\"{join_table}\"")];

    for (source, alias) in each_joins {
        parts.push(conn.json_each_source(source, alias));
    }

    parts.join(", ")
}

/// [`row_select`] wrapped in `EXISTS` — some row satisfies `op_sql`.
fn exists_row(
    conn: &dyn DbConnection,
    scope: &SubqueryScope<'_>,
    from: Option<&str>,
    op_sql: &str,
    params: &mut Vec<DbValue>,
) -> String {
    format!("EXISTS ({})", row_select(conn, scope, from, op_sql, params))
}

/// `SELECT 1` over the scope's rows of one parent document that satisfy
/// `op_sql`, constrained to the scope's locale.
///
/// `from` is the FROM list of a JSON subquery, whose `json_each` expansions
/// expose columns of their own — its `parent_id` is qualified by the join
/// table there. `None` reads the join table alone.
fn row_select(
    conn: &dyn DbConnection,
    scope: &SubqueryScope<'_>,
    from: Option<&str>,
    op_sql: &str,
    params: &mut Vec<DbValue>,
) -> String {
    let SubqueryScope {
        join_table,
        parent_table,
        locale_constraint,
    } = *scope;
    let locale_sql = append_locale_clause(conn, join_table, locale_constraint, params);

    let (from, parent_id) = match from {
        Some(from) => (from.to_string(), format!("\"{join_table}\".parent_id")),
        None => (format!("\"{join_table}\""), "parent_id".to_string()),
    };

    format!(
        "SELECT 1 FROM {from} WHERE {parent_id} = \"{parent_table}\".id AND {op_sql}{locale_sql}"
    )
}

/// Produce the trailing `AND "{join_table}"._locale = ?` fragment and push
/// the locale bind parameter, or return `""` when no locale constraint applies.
///
/// A filter on a localized junction table (an array, blocks or has-many
/// relationship whose field is localized) only matches rows belonging to the
/// active locale.
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

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::db::InMemoryConn;

    fn scope(locale: Option<&'static str>) -> SubqueryScope<'static> {
        SubqueryScope {
            join_table: "posts_items",
            parent_table: "posts",
            locale_constraint: locale,
        }
    }

    fn filter(field: &str, op: FilterOp) -> Filter {
        Filter {
            field: field.to_string(),
            op,
        }
    }

    /// A negative operator on a has-many junction asks that no row hold the
    /// id; the locale constraint binds after the operand, inside the subquery.
    #[test]
    fn a_negative_related_id_filter_is_not_exists() {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();

        let sql = build_subquery_sql(
            &conn,
            &scope(Some("de")),
            &SubqueryCondition::RelatedId,
            &filter("tags.id", FilterOp::NotEquals("t1".into())),
            &mut params,
        )
        .unwrap();

        assert_eq!(
            sql,
            "NOT EXISTS (SELECT 1 FROM \"posts_items\" WHERE parent_id = \"posts\".id AND \"related_id\" = ?1 AND \"posts_items\"._locale = ?2)"
        );
        assert_eq!(
            params,
            vec![DbValue::Text("t1".into()), DbValue::Text("de".into())]
        );
    }

    /// A scalar has-many sub-field of an array row: some row whose own list
    /// matches, the list read qualified by the join table.
    #[test]
    fn an_array_list_column_quantifies_inside_the_row() {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();
        let condition = SubqueryCondition::Column {
            col: "tags".to_string(),
            field_type: Some(FieldType::Text),
            list: Some(ListLeaf::Scalar(FieldType::Text)),
        };

        let sql = build_subquery_sql(
            &conn,
            &scope(None),
            &condition,
            &filter("items.tags", FilterOp::Equals("a".into())),
            &mut params,
        )
        .unwrap();

        assert!(
            sql.starts_with(
                "EXISTS (SELECT 1 FROM \"posts_items\" WHERE parent_id = \"posts\".id AND EXISTS (SELECT 1 FROM json_each(\"posts_items\".\"tags\") AS crap_el"
            ),
            "{sql}"
        );
        assert!(
            sql.ends_with("AS crap_el WHERE crap_el.value = ?1))"),
            "{sql}"
        );
    }

    /// A scalar has-many value inside a block's JSON expands the extracted list.
    #[test]
    fn a_json_list_value_expands_the_extracted_array() {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();
        let condition = SubqueryCondition::Json {
            each_joins: vec![],
            extract_expr: "json_extract(data, '$.tags')".to_string(),
            field_type: Some(FieldType::Number),
            list: Some(ListLeaf::Scalar(FieldType::Number)),
        };

        let sql = build_subquery_sql(
            &conn,
            &scope(None),
            &condition,
            &filter("content.tags", FilterOp::NotIn(vec!["1".into()])),
            &mut params,
        )
        .unwrap();

        assert!(
            sql.starts_with(
                "EXISTS (SELECT 1 FROM \"posts_items\" WHERE \"posts_items\".parent_id = \"posts\".id AND NOT EXISTS (SELECT 1 FROM json_each(json_extract(data, '$.tags')) AS crap_el"
            ),
            "{sql}"
        );
        assert_eq!(params, vec![DbValue::Real(1.0)]);
    }

    /// Rows of an array are records, not list elements: a negative operator
    /// on a plain sub-field still asks for some row that satisfies it.
    #[test]
    fn a_plain_array_sub_field_keeps_the_some_row_reading() {
        let conn = InMemoryConn::open();
        let mut params = Vec::new();
        let condition = SubqueryCondition::Column {
            col: "name".to_string(),
            field_type: Some(FieldType::Text),
            list: None,
        };

        let sql = build_subquery_sql(
            &conn,
            &scope(None),
            &condition,
            &filter("items.name", FilterOp::NotEquals("x".into())),
            &mut params,
        )
        .unwrap();

        assert_eq!(
            sql,
            "EXISTS (SELECT 1 FROM \"posts_items\" WHERE parent_id = \"posts\".id AND \"name\" != ?1)"
        );
    }
}
