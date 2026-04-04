//! Database table introspection helpers.

use anyhow::Result;
use std::collections::{HashMap, HashSet};

use crate::db::DbConnection;

/// Check if a table exists in the database.
pub fn table_exists(conn: &dyn DbConnection, name: &str) -> Result<bool> {
    conn.table_exists(name)
}

/// Get the set of column names for a table.
pub fn get_table_columns(conn: &dyn DbConnection, table: &str) -> Result<HashSet<String>> {
    conn.get_table_columns(table)
}

/// Get a mapping of column name -> column type for a table.
pub fn get_table_column_types(
    conn: &dyn DbConnection,
    table: &str,
) -> Result<HashMap<String, String>> {
    conn.get_table_column_types(table)
}

pub use crate::db::query::sanitize_locale;

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
}
