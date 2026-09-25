//! Drop the `NOT NULL` older releases put on required user-field columns.
//!
//! **One-time conversion — removable after 0.1.0**
//!
//! A collection table used to be created with `NOT NULL` on the column of a
//! `required` field (its default-locale column when localized) unless the
//! collection had drafts, and nothing reconciled it afterwards. Removing
//! `required`, enabling drafts later or removing the field then left every
//! write that omits the value failing at the database. User-field columns are
//! never created `NOT NULL` any more — validation owns `required` — and this
//! pass relaxes the tables created before that.
//!
//! Detection and the gate live here; carrying the change out belongs to the
//! collection sync (Postgres drops the constraint in place, `SQLite` rebuilds
//! the table), which is also where the gate is stamped. The gate is per
//! collection — `nullable_columns:{slug}` holding [`VERSION`] — so a
//! collection whose old table comes back into the registry later is still
//! relaxed.

use anyhow::{Context as _, Result};

use crate::db::{DbConnection, DbValue, query::helpers::quote_ident};

use super::{helpers::table_exists, meta};

/// The gate's version. Bump it to run the pass again on every database.
const VERSION: &str = "1";

/// The columns a collection table declares `NOT NULL` on purpose: the primary
/// key and the system columns that always hold a value.
const SYSTEM_NOT_NULL: &[&str] = &["id", "_status", "_ref_count"];

/// The `_crap_meta` key gating the pass for one collection.
fn gate_key(slug: &str) -> String {
    format!("nullable_columns:{slug}")
}

/// Whether the pass still has to run for `slug`. A database without
/// `_crap_meta` yet has run nothing.
fn pending(conn: &dyn DbConnection, slug: &str) -> Result<bool> {
    if !table_exists(conn, "_crap_meta")? {
        return Ok(true);
    }

    Ok(meta::get(conn, &gate_key(slug))?.as_deref() != Some(VERSION))
}

/// The columns of `table` declared `NOT NULL`, the primary key excluded.
fn not_null_columns(conn: &dyn DbConnection, table: &str) -> Result<Vec<String>> {
    let p1 = conn.placeholder(1);

    let sql = if conn.is_postgres() {
        format!(
            "SELECT column_name AS name FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = {p1} AND is_nullable = 'NO'"
        )
    } else {
        format!("SELECT name FROM pragma_table_info({p1}) WHERE \"notnull\" = 1 AND pk = 0")
    };

    let rows = conn
        .query_all(&sql, &[DbValue::Text(table.to_string())])
        .with_context(|| format!("Failed to read the NOT NULL columns of '{table}'"))?;

    Ok(rows
        .iter()
        .filter_map(|row| row.get_string("name").ok())
        .collect())
}

/// The user-field columns of collection `slug` that still carry `NOT NULL`,
/// sorted — `None` once the pass has run for it. `Some(empty)` means the pass
/// is pending but the table has nothing to relax; the gate still has to be
/// stamped.
pub(in crate::db::migrate) fn columns_to_relax(
    conn: &dyn DbConnection,
    slug: &str,
) -> Result<Option<Vec<String>>> {
    if !pending(conn, slug)? {
        return Ok(None);
    }

    let mut columns: Vec<String> = not_null_columns(conn, slug)?
        .into_iter()
        .filter(|col| !SYSTEM_NOT_NULL.contains(&col.as_str()))
        .collect();
    columns.sort();

    Ok(Some(columns))
}

/// Drop `NOT NULL` from `columns` of `table` in place — Postgres, whose
/// `ALTER TABLE` can.
pub(in crate::db::migrate) fn drop_not_null(
    conn: &dyn DbConnection,
    table: &str,
    columns: &[String],
) -> Result<()> {
    for col in columns {
        let sql = format!(
            "ALTER TABLE {} ALTER COLUMN {} DROP NOT NULL",
            quote_ident(table),
            quote_ident(col)
        );

        conn.execute_ddl(&sql, &[])
            .with_context(|| format!("Failed to drop NOT NULL from '{table}.{col}'"))?;
    }

    Ok(())
}

/// Record that the pass ran for `slug`.
pub(in crate::db::migrate) fn mark_relaxed(conn: &dyn DbConnection, slug: &str) -> Result<()> {
    meta::upsert(conn, &gate_key(slug), VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate::collection::test_helpers::in_memory_pool;

    /// The primary key and the system columns keep their `NOT NULL`; a user
    /// column is reported, sorted.
    #[test]
    fn reports_only_user_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL, \
             body TEXT, alpha INTEGER NOT NULL DEFAULT 0, \
             _status TEXT NOT NULL DEFAULT 'published', _ref_count INTEGER NOT NULL DEFAULT 0)",
        )
        .unwrap();

        assert_eq!(
            columns_to_relax(&conn, "posts").unwrap(),
            Some(vec!["alpha".to_string(), "title".to_string()])
        );
    }

    /// Once stamped, the pass reports nothing for that collection — and only
    /// for that one.
    #[test]
    fn the_gate_is_per_collection() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT NOT NULL);
             CREATE TABLE pages (id TEXT PRIMARY KEY, title TEXT NOT NULL);",
        )
        .unwrap();

        mark_relaxed(&conn, "posts").unwrap();

        assert_eq!(columns_to_relax(&conn, "posts").unwrap(), None);
        assert_eq!(
            columns_to_relax(&conn, "pages").unwrap(),
            Some(vec!["title".to_string()])
        );
    }
}
