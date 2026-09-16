//! The paged reader a one-time conversion scans a table with, and the rewrite
//! it applies to a row it visits.
//!
//! A conversion runs inside the migration transaction, which holds the
//! schema-sync lock for as long as it does, so a `SELECT` without a limit would
//! materialise every row of a table before the first one is rewritten. Reading a
//! page at a time keeps the memory a scan needs bounded however large the table,
//! and every conversion scans through this one reader so none of them can drift
//! back to an unbounded read.

use std::fmt::Write as _;

use anyhow::{Context as _, Result};

use crate::{
    core::Builder,
    db::{
        DbConnection, DbRow, DbValue, migrate::helpers::table_exists, query::helpers::quote_ident,
    },
};

/// Rows read per page.
pub(in crate::db::migrate) const PAGE_SIZE: usize = 500;

/// What a scan reads: `id` and `columns` of `table`, of the rows whose last of
/// `columns` isn't NULL.
#[derive(Builder)]
pub(in crate::db::migrate) struct Scan<'a> {
    #[builder(required)]
    table: &'a str,
    #[builder(required)]
    columns: &'a [&'a str],
    /// Leave soft-deleted rows out.
    #[builder(default = false)]
    active_only: bool,
}

/// Visit every row of a scan, a page at a time in id order.
///
/// Keyset paging — each page starts after the last id of the one before — keeps
/// every read bounded however large the table. A row rewritten during the visit
/// keeps its id, so the walk neither visits it twice nor steps over its
/// neighbour. A table that doesn't exist has no rows to visit.
///
/// # Errors
///
/// Returns a backend error if introspection or a page read fails, or whatever
/// `visit` returns.
pub(in crate::db::migrate) fn for_each_row(
    conn: &dyn DbConnection,
    scan: &Scan<'_>,
    visit: &mut dyn FnMut(&DbRow) -> Result<()>,
) -> Result<()> {
    if scan.columns.is_empty() || !table_exists(conn, scan.table)? {
        return Ok(());
    }

    let mut after: Option<String> = None;

    loop {
        let rows = read_page(conn, scan, after.as_deref())?;

        for row in &rows {
            visit(row)?;
        }

        if rows.len() < PAGE_SIZE {
            return Ok(());
        }

        let Some(last) = rows.last().and_then(|row| row.opt_text_at(0)) else {
            return Ok(());
        };
        after = Some(last);
    }
}

/// The UPDATE storing a converted value (placeholder 1) in `column` of the row
/// with an id (placeholder 2) — the rewrite every scanning conversion applies.
#[must_use]
pub(in crate::db::migrate) fn update_by_id(
    conn: &dyn DbConnection,
    table: &str,
    column: &str,
) -> String {
    format!(
        "UPDATE {} SET {} = {} WHERE id = {}",
        quote_ident(table),
        quote_ident(column),
        conn.placeholder(1),
        conn.placeholder(2)
    )
}

/// One page of [`for_each_row`]: up to [`PAGE_SIZE`] rows with an id after
/// `after` (from the first row without one).
fn read_page(conn: &dyn DbConnection, scan: &Scan<'_>, after: Option<&str>) -> Result<Vec<DbRow>> {
    let Some(required) = scan.columns.last() else {
        return Ok(Vec::new());
    };

    let selected: Vec<String> = scan.columns.iter().copied().map(quote_ident).collect();
    let mut sql = format!(
        "SELECT id, {} FROM {} WHERE {} IS NOT NULL",
        selected.join(", "),
        quote_ident(scan.table),
        quote_ident(required)
    );

    if scan.active_only {
        sql.push_str(" AND _deleted_at IS NULL");
    }

    let mut params = Vec::new();

    if let Some(after) = after {
        let _ = write!(sql, " AND id > {}", conn.placeholder(1));
        params.push(DbValue::Text(after.to_string()));
    }

    let _ = write!(sql, " ORDER BY id LIMIT {PAGE_SIZE}");

    conn.query_all(&sql, &params)
        .with_context(|| format!("Failed to read {}", scan.table))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::InMemoryConn;

    fn conn_with_rows(rows: usize) -> InMemoryConn {
        let conn = InMemoryConn::open();
        conn.0
            .execute_batch("CREATE TABLE posts (id TEXT PRIMARY KEY, body TEXT, _deleted_at TEXT);")
            .unwrap();

        for i in 0..rows {
            conn.0
                .execute(
                    "INSERT INTO posts (id, body) VALUES (?1, 'v')",
                    [format!("r{i:05}")],
                )
                .unwrap();
        }

        conn
    }

    fn visited(conn: &InMemoryConn, scan: &Scan<'_>) -> Vec<String> {
        let mut ids = Vec::new();

        for_each_row(conn, scan, &mut |row| {
            ids.extend(row.opt_text_at(0));

            Ok(())
        })
        .unwrap();

        ids
    }

    /// Every row is visited once, in id order, however many pages it takes.
    #[test]
    fn visits_every_row_past_the_first_page() {
        let rows = PAGE_SIZE * 2 + 1;
        let conn = conn_with_rows(rows);

        let ids = visited(&conn, &Scan::builder("posts", &["body"]).build());

        let expected: Vec<String> = (0..rows).map(|i| format!("r{i:05}")).collect();
        assert_eq!(ids, expected, "every row once, in id order");
    }

    /// A row whose last column is NULL is left out, and so is a soft-deleted
    /// row when the scan asks for the active ones.
    #[test]
    fn leaves_out_null_and_soft_deleted_rows() {
        let conn = conn_with_rows(0);
        conn.0
            .execute_batch(
                "INSERT INTO posts VALUES ('kept', 'v', NULL);
                 INSERT INTO posts VALUES ('null', NULL, NULL);
                 INSERT INTO posts VALUES ('trashed', 'v', '2026-01-01T00:00:00.000Z');",
            )
            .unwrap();

        assert_eq!(
            visited(&conn, &Scan::builder("posts", &["body"]).build()),
            vec!["kept".to_string(), "trashed".to_string()]
        );
        assert_eq!(
            visited(
                &conn,
                &Scan::builder("posts", &["body"]).active_only(true).build()
            ),
            vec!["kept".to_string()]
        );
    }

    /// A table that isn't there yet has no rows to visit, rather than failing
    /// the whole migration.
    #[test]
    fn a_missing_table_has_no_rows() {
        let conn = conn_with_rows(0);

        assert!(visited(&conn, &Scan::builder("absent", &["body"]).build()).is_empty());
    }
}
