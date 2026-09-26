//! Database table introspection helpers.

use anyhow::{Context as _, Result};
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::db::{DbConnection, DbValue};

/// Check if a table exists in the database.
pub(crate) fn table_exists(conn: &dyn DbConnection, name: &str) -> Result<bool> {
    conn.table_exists(name)
}

/// Get the set of column names for a table.
pub(crate) fn get_table_columns(conn: &dyn DbConnection, table: &str) -> Result<HashSet<String>> {
    conn.get_table_columns(table)
}

/// Get a mapping of column name -> column type for a table.
pub(crate) fn get_table_column_types(
    conn: &dyn DbConnection,
    table: &str,
) -> Result<HashMap<String, String>> {
    conn.get_table_column_types(table)
}

pub use crate::db::query::sanitize_locale;

/// Which kind of table constraint [`table_constraints`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConstraintKind {
    PrimaryKey,
    /// A `UNIQUE` written into the table definition — not a unique index
    /// created on its own, which the catalogs keep apart.
    Unique,
}

/// A primary key or inline `UNIQUE` constraint as the catalog stores it: its
/// name (on `SQLite` the name of its automatic index, empty for a primary
/// key) and the columns it spans, in key order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableConstraint {
    pub name: String,
    pub columns: Vec<String>,
}

impl TableConstraint {
    pub(crate) fn new(name: String, columns: Vec<String>) -> Self {
        Self { name, columns }
    }
}

/// The constraints of `kind` a table carries, read from the catalog — the
/// stored shape a reconcile compares with the shape a definition wants.
///
/// # Errors
///
/// Returns a backend error if a catalog query fails.
pub(crate) fn table_constraints(
    conn: &dyn DbConnection,
    table: &str,
    kind: ConstraintKind,
) -> Result<Vec<TableConstraint>> {
    let constraints = if conn.is_postgres() {
        pg_constraints(conn, table, kind)
    } else if kind == ConstraintKind::PrimaryKey {
        sqlite_primary_key(conn, table)
    } else {
        sqlite_inline_unique(conn, table)
    };

    constraints.with_context(|| format!("Failed to read the constraints of '{table}'"))
}

fn pg_constraints(
    conn: &dyn DbConnection,
    table: &str,
    kind: ConstraintKind,
) -> Result<Vec<TableConstraint>> {
    let constraint_type = match kind {
        ConstraintKind::PrimaryKey => "PRIMARY KEY",
        ConstraintKind::Unique => "UNIQUE",
    };

    let rows = conn.query_all(
        "SELECT tc.constraint_name, kcu.column_name FROM information_schema.table_constraints tc \
         JOIN information_schema.key_column_usage kcu \
           ON kcu.constraint_schema = tc.constraint_schema \
          AND kcu.constraint_name = tc.constraint_name \
          AND kcu.table_name = tc.table_name \
         WHERE tc.table_schema = 'public' AND tc.table_name = $1 AND tc.constraint_type = $2 \
         ORDER BY tc.constraint_name, kcu.ordinal_position",
        &[
            DbValue::Text(table.to_string()),
            DbValue::Text(constraint_type.to_string()),
        ],
    )?;

    let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for row in &rows {
        by_name
            .entry(row.get_string("constraint_name")?)
            .or_default()
            .push(row.get_string("column_name")?);
    }

    Ok(by_name
        .into_iter()
        .map(|(name, columns)| TableConstraint::new(name, columns))
        .collect())
}

fn sqlite_primary_key(conn: &dyn DbConnection, table: &str) -> Result<Vec<TableConstraint>> {
    let rows = conn.query_all(
        "SELECT name FROM pragma_table_info(?1) WHERE pk > 0 ORDER BY pk",
        &[DbValue::Text(table.to_string())],
    )?;
    let columns: Vec<String> = rows.iter().filter_map(|r| r.opt_text_at(0)).collect();

    if columns.is_empty() {
        return Ok(Vec::new());
    }

    Ok(vec![TableConstraint::new(String::new(), columns)])
}

fn sqlite_inline_unique(conn: &dyn DbConnection, table: &str) -> Result<Vec<TableConstraint>> {
    let indexes = conn.query_all(
        "SELECT name FROM pragma_index_list(?1) WHERE origin = 'u' ORDER BY name",
        &[DbValue::Text(table.to_string())],
    )?;

    let mut constraints = Vec::with_capacity(indexes.len());

    for name in indexes.iter().filter_map(|r| r.opt_text_at(0)) {
        let rows = conn.query_all(
            "SELECT name FROM pragma_index_info(?1) ORDER BY seqno",
            &[DbValue::Text(name.clone())],
        )?;
        let columns = rows.iter().filter_map(|r| r.opt_text_at(0)).collect();

        constraints.push(TableConstraint::new(name, columns));
    }

    Ok(constraints)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate::collection::test_helpers::*;

    #[test]
    fn table_exists_false_initially() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        assert!(!table_exists(&conn, "nonexistent").unwrap());
    }

    #[test]
    fn table_exists_true_after_create() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE test_table (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        assert!(table_exists(&conn, "test_table").unwrap());
    }

    #[test]
    fn get_table_columns_returns_column_names() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE t (id TEXT, name TEXT, age INTEGER)", &[])
            .unwrap();
        let cols = get_table_columns(&conn, "t").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("name"));
        assert!(cols.contains("age"));
        assert_eq!(cols.len(), 3);
    }

    /// The catalog reports a composite primary key in key order, and nothing
    /// for a table without one.
    #[test]
    fn primary_key_columns_are_read_in_key_order() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE j (a TEXT, b TEXT, c TEXT, PRIMARY KEY (b, a)); CREATE TABLE n (a TEXT);",
        )
        .unwrap();

        let pk = table_constraints(&conn, "j", ConstraintKind::PrimaryKey).unwrap();
        assert_eq!(pk.len(), 1);
        assert_eq!(pk[0].columns, vec!["b", "a"]);

        assert!(
            table_constraints(&conn, "n", ConstraintKind::PrimaryKey)
                .unwrap()
                .is_empty()
        );
    }

    /// Inline `UNIQUE` constraints are reported with their columns; a unique
    /// index created on its own and the primary key are not.
    #[test]
    fn inline_unique_constraints_are_told_apart_from_unique_indexes() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE t (id TEXT PRIMARY KEY, slug TEXT UNIQUE, a TEXT, b TEXT, \
             code TEXT, UNIQUE (a, b)); \
             CREATE UNIQUE INDEX idx_t_code_unique ON t (code);",
        )
        .unwrap();

        let mut columns: Vec<Vec<String>> = table_constraints(&conn, "t", ConstraintKind::Unique)
            .unwrap()
            .into_iter()
            .map(|c| c.columns)
            .collect();
        columns.sort();

        assert_eq!(columns, vec![vec!["a", "b"], vec!["slug"]]);
    }
}
