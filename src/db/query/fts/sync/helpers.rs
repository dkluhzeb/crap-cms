//! FTS helpers shared by migration + upsert.

#[cfg(test)]
use crate::db::query::fts::search::table_exists;
use crate::db::query::fts::{
    index::ColumnText,
    layout::{FtsColumn, PG_FTS_CONFIG, PgVector},
};
#[cfg(test)]
use crate::db::{DbConnection, DbValue};

/// Build a Postgres `to_tsvector('simple', <text>)` expression over `text_expr`
/// (a bound placeholder or a SQL text expression).
pub(super) fn pg_tsvector(text_expr: &str) -> String {
    format!("to_tsvector('{PG_FTS_CONFIG}', {text_expr})")
}

/// Each index column's indexable text, as [`ColumnText`] reads its stored
/// value `raw(i)` (`''` when absent). Shared by the per-write upsert and the
/// startup rebuild, so both index the same words.
pub(super) fn column_texts<'r>(
    columns: &[FtsColumn],
    column_text: &ColumnText<'_>,
    raw: impl Fn(usize) -> Option<&'r str>,
) -> Vec<String> {
    columns
        .iter()
        .enumerate()
        .map(|(i, column)| column_text.text(&column.name, raw(i).unwrap_or("")))
        .collect()
}

/// The text each Postgres tsvector is built from: its member columns' texts,
/// space-joined, in `vectors` order.
pub(super) fn pg_vector_texts<'a>(
    vectors: &'a [PgVector],
    texts: &'a [String],
) -> impl Iterator<Item = String> + 'a {
    vectors.iter().map(|vector| {
        vector
            .members
            .iter()
            .map(|&i| texts[i].as_str())
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// Get column names from the FTS table (excludes `id`) — a test probe for the
/// rebuilt table's shape. Returns `None` if the FTS table doesn't exist or has
/// no columns (Postgres has only `tsv`, so it always yields `None` there).
#[cfg(test)]
pub(super) fn get_fts_table_columns(
    conn: &dyn DbConnection,
    fts_table: &str,
) -> Option<Vec<String>> {
    if !table_exists(conn, fts_table) {
        return None;
    }

    if conn.is_postgres() {
        let p1 = conn.placeholder(1);
        let sql = format!(
            "SELECT column_name FROM information_schema.columns \
             WHERE table_schema='public' AND table_name={p1} AND column_name != 'id'"
        );

        let rows = conn
            .query_all(&sql, &[DbValue::Text(fts_table.to_string())])
            .ok()?;

        let cols: Vec<String> = rows
            .into_iter()
            .filter_map(|row| row.opt_text_at(0))
            .collect();

        if cols.is_empty() { None } else { Some(cols) }
    } else {
        // Use PRAGMA table_info (not table_xinfo) — table_xinfo includes hidden
        // virtual columns like the table name and rank which aren't real data columns.
        let rows = conn
            .query_all(&format!("PRAGMA table_info({fts_table})"), &[])
            .ok()?;

        let cols: Vec<String> = rows
            .into_iter()
            .filter_map(|row| row.text_at(1).filter(|n| *n != "id").map(str::to_string))
            .collect();

        if cols.is_empty() { None } else { Some(cols) }
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::db::InMemoryConn;

    #[test]
    fn missing_table_returns_none() {
        let conn = InMemoryConn::open();
        assert_eq!(get_fts_table_columns(&conn, "nope_fts"), None);
    }

    #[test]
    fn returns_columns_in_order_excluding_id() {
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE posts_fts (id TEXT, title TEXT, body TEXT);");
        assert_eq!(
            get_fts_table_columns(&conn, "posts_fts"),
            Some(vec!["title".to_string(), "body".to_string()])
        );
    }

    #[test]
    fn id_only_table_returns_none() {
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE bare_fts (id TEXT);");
        assert_eq!(get_fts_table_columns(&conn, "bare_fts"), None);
    }
}
