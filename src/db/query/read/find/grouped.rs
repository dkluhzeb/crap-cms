//! [`find_grouped`] — a find bounded per group: at most N documents for each
//! distinct value of one column, in the query's sort order. The reverse lookup
//! of a join field reads its children this way, so one query serves a whole
//! page of parents while no parent lists more than the join's limit.

use anyhow::{Context as _, Result};

use super::runner::{apply_soft_delete, build_select_named, map_rows};
use super::sort::{apply_order_by, resolve_sort};
use crate::core::{Builder, CollectionDefinition, Document};
use crate::db::query::filter::build_where_clause;
use crate::db::query::{column_read_expr, helpers::quote_ident, validate_query_fields};
use crate::db::{DbConnection, DbValue, FindQuery, LocaleContext};

/// The row-number column the window adds; `_`-prefixed, so no field can own it.
const ROW_NUMBER: &str = "_group_row";

/// Which column documents are grouped by, and how many each group keeps.
pub struct GroupLimit<'a> {
    pub column: &'a str,
    pub per_group: i64,
}

impl<'a> GroupLimit<'a> {
    #[must_use]
    pub fn new(column: &'a str, per_group: i64) -> Self {
        Self { column, per_group }
    }
}

/// One grouped find: the collection read, the query, and the grouping.
#[derive(Builder)]
pub struct GroupedFind<'a> {
    #[builder(required)]
    pub slug: &'a str,
    #[builder(required)]
    pub def: &'a CollectionDefinition,
    #[builder(required)]
    pub query: &'a FindQuery,
    #[builder(required)]
    pub group: GroupLimit<'a>,
    pub locale_ctx: Option<&'a LocaleContext>,
}

/// Find the documents matching the query's filters, keeping at most
/// `group.per_group` for each distinct value of `group.column` — the first ones
/// in the query's sort order (the collection default when it names none).
/// Within a group the result keeps that order; groups come back interleaved.
///
/// The query's filters, sort, `select` and `include_deleted` apply as in
/// [`find`](super::find); its search, cursors, limit and offset do not.
///
/// # Errors
///
/// Returns an error if a filter/sort field or the group column is invalid, or
/// a backend error if the SELECT fails.
pub fn find_grouped(conn: &dyn DbConnection, find: &GroupedFind<'_>) -> Result<Vec<Document>> {
    let (slug, def, locale_ctx) = (find.slug, find.def, find.locale_ctx);

    validate_query_fields(def, find.query, locale_ctx)?;

    let mut params: Vec<DbValue> = Vec::new();
    let (inner, names) = grouped_select(conn, find, &mut params)?;

    let limit = conn.placeholder(params.len() + 1);
    params.push(DbValue::Integer(find.group.per_group.max(0)));

    let columns: Vec<String> = names.iter().map(|n| quote_ident(n)).collect();
    let sql = format!(
        "SELECT {} FROM ({inner}) AS \"_grouped\" WHERE {ROW_NUMBER} <= {limit} ORDER BY {ROW_NUMBER}",
        columns.join(", ")
    );

    let rows = conn
        .query_all(&sql, &params)
        .with_context(|| format!("Failed to execute grouped query on '{slug}'"))?;

    map_rows(conn, &rows, locale_ctx, def, false)
}

/// The inner SELECT: every matching row, numbered within its group in sort
/// order. Returns the SQL and the names its document columns come back under.
fn grouped_select(
    conn: &dyn DbConnection,
    find: &GroupedFind<'_>,
    params: &mut Vec<DbValue>,
) -> Result<(String, Vec<String>)> {
    let (slug, def, query, locale_ctx) = (find.slug, find.def, find.query, find.locale_ctx);

    let (exprs, names) = build_select_named(def, query, locale_ctx)?;
    let partition = column_read_expr(find.group.column, &def.fields, locale_ctx)?;

    let (sort_col, sort_dir, _) = resolve_sort(def, query)?;
    let mut order = String::new();
    apply_order_by(&sort_col, sort_dir, false, def, locale_ctx, &mut order)?;

    let mut sql = format!(
        "SELECT {}, ROW_NUMBER() OVER (PARTITION BY {partition}{order}) AS {ROW_NUMBER} FROM \"{slug}\"",
        exprs.join(", ")
    );

    let where_clause =
        build_where_clause(conn, &query.filters, slug, &def.fields, locale_ctx, params)?;
    let mut has_where = !where_clause.is_empty();
    sql.push_str(&where_clause);

    apply_soft_delete(def, query, &mut sql, &mut has_where);

    Ok((sql, names))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::super::test_helpers::{setup_db, test_def};
    use super::*;
    use crate::db::{Filter, FilterClause, FilterOp};

    /// `posts` holding four rows under status `a` and two under `b`.
    fn seeded() -> (tempfile::TempDir, crate::db::DbPool) {
        let (tmp, pool) = setup_db();

        pool.get()
            .unwrap()
            .execute_batch(
                "INSERT INTO posts (id, title, status) VALUES
                    ('p1', 'd', 'a'), ('p2', 'b', 'a'), ('p3', 'a', 'a'), ('p4', 'c', 'a'),
                    ('p5', 'z', 'b'), ('p6', 'y', 'b');",
            )
            .unwrap();

        (tmp, pool)
    }

    fn titles(docs: &[Document], status: &str) -> Vec<String> {
        docs.iter()
            .filter(|d| d.get_str("status") == Some(status))
            .filter_map(|d| d.get_str("title").map(str::to_string))
            .collect()
    }

    /// Each group keeps its first `per_group` rows in sort order, and the
    /// filters apply before the grouping.
    #[test]
    fn keeps_the_first_rows_of_each_group_in_sort_order() {
        let (_tmp, pool) = seeded();
        let conn = pool.get().unwrap();
        let def = test_def();

        let query = FindQuery::builder()
            .filters(vec![FilterClause::Single(Filter {
                field: "status".to_string(),
                op: FilterOp::In(vec!["a".into(), "b".into()]),
            })])
            .order_by(Some("title".to_string()))
            .build();

        let find =
            GroupedFind::builder("posts", &def, &query, GroupLimit::new("status", 2)).build();
        let docs = find_grouped(&conn, &find).unwrap();

        assert_eq!(titles(&docs, "a"), vec!["a", "b"]);
        assert_eq!(titles(&docs, "b"), vec!["y", "z"]);
        assert_eq!(docs.len(), 4);
        assert!(
            docs.iter().all(|d| d.fields.get(ROW_NUMBER).is_none()),
            "the row number is not a document field"
        );
    }

    #[test]
    fn a_filter_narrows_before_the_group_limit() {
        let (_tmp, pool) = seeded();
        let conn = pool.get().unwrap();
        let def = test_def();

        let query = FindQuery::builder()
            .filters(vec![FilterClause::Single(Filter {
                field: "title".to_string(),
                op: FilterOp::NotEquals("a".into()),
            })])
            .order_by(Some("title".to_string()))
            .build();

        let find =
            GroupedFind::builder("posts", &def, &query, GroupLimit::new("status", 2)).build();
        let docs = find_grouped(&conn, &find).unwrap();

        assert_eq!(titles(&docs, "a"), vec!["b", "c"]);
    }
}
