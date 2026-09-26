//! Drop the inline `UNIQUE` constraints older releases put on unique fields.
//!
//! **One-time conversion — removable after 0.1.0**
//!
//! A collection table used to be created with an inline `UNIQUE` on the
//! column(s) of every `unique` field unless the collection used soft delete.
//! Uniqueness now lives only in the managed unique indexes the schema sync
//! creates and drops with the definition; an inline constraint can't follow a
//! definition change, so removing `unique` from such a field left every write
//! of a duplicate failing at the database, and turning `soft_delete` on later
//! left a trashed row blocking a new one with its value. This pass drops those
//! constraints once per collection, on both backends alike.
//!
//! Detection (from the catalog) and the gate live here; carrying the change
//! out belongs to the collection sync (Postgres drops the constraints in
//! place, `SQLite` rebuilds the table), which is also where the gate is
//! stamped. The gate is per collection — `inline_unique:{slug}` holding
//! [`VERSION`] — so a collection whose old table comes back into the registry
//! later is still converted, and a constraint an operator adds by hand after
//! the pass ran is left alone.

use anyhow::Result;

use crate::db::DbConnection;

use super::{
    helpers::{ConstraintKind, table_constraints, table_exists},
    meta,
};

/// The gate's version. Bump it to run the pass again on every database.
const VERSION: &str = "1";

/// The `_crap_meta` key gating the pass for one collection.
fn gate_key(slug: &str) -> String {
    format!("inline_unique:{slug}")
}

/// Whether the pass still has to run for `slug`. A database without
/// `_crap_meta` yet has run nothing.
fn pending(conn: &dyn DbConnection, slug: &str) -> Result<bool> {
    if !table_exists(conn, "_crap_meta")? {
        return Ok(true);
    }

    Ok(meta::get(conn, &gate_key(slug))?.as_deref() != Some(VERSION))
}

/// Whether collection `slug`'s table still carries an inline `UNIQUE`
/// constraint to drop — `None` once the pass has run for it. `Some(false)`
/// means the pass is pending but the table has none; the gate still has to be
/// stamped.
pub(in crate::db::migrate) fn inline_unique_to_drop(
    conn: &dyn DbConnection,
    slug: &str,
) -> Result<Option<bool>> {
    if !pending(conn, slug)? {
        return Ok(None);
    }

    let constraints = table_constraints(conn, slug, ConstraintKind::Unique)?;

    Ok(Some(!constraints.is_empty()))
}

/// Record that the pass ran for `slug`.
pub(in crate::db::migrate) fn mark_dropped(conn: &dyn DbConnection, slug: &str) -> Result<()> {
    meta::upsert(conn, &gate_key(slug), VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate::collection::test_helpers::in_memory_pool;

    /// A table with an inline `UNIQUE` has one to drop; one without has none.
    #[test]
    fn reports_an_inline_unique_constraint() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, slug TEXT UNIQUE);
             CREATE TABLE pages (id TEXT PRIMARY KEY, slug TEXT);
             CREATE UNIQUE INDEX idx_pages_slug_unique ON pages (slug);",
        )
        .unwrap();

        assert_eq!(inline_unique_to_drop(&conn, "posts").unwrap(), Some(true));
        assert_eq!(
            inline_unique_to_drop(&conn, "pages").unwrap(),
            Some(false),
            "a managed unique index is not an inline constraint"
        );
    }

    /// Once stamped, the pass reports nothing for that collection — and only
    /// for that one.
    #[test]
    fn the_gate_is_per_collection() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, slug TEXT UNIQUE);
             CREATE TABLE pages (id TEXT PRIMARY KEY, slug TEXT UNIQUE);",
        )
        .unwrap();

        mark_dropped(&conn, "posts").unwrap();

        assert_eq!(inline_unique_to_drop(&conn, "posts").unwrap(), None);
        assert_eq!(inline_unique_to_drop(&conn, "pages").unwrap(), Some(true));
    }
}
