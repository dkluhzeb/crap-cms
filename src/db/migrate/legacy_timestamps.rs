//! **One-time conversion — removable after 0.1.0** (see [`super::one_time`]).
//!
//! One-time rewrite of legacy space-separated timestamps to ISO 8601 on disk.
//!
//! Early `SQLite` schemas defaulted timestamp columns to `datetime('now')`, which
//! stores `YYYY-MM-DD HH:MM:SS`. Every current writer stores
//! `YYYY-MM-DDTHH:MM:SS.sssZ`. Reads normalize the old form, but SQL does not:
//! `ORDER BY`, keyset cursors and `<`/`>` filters compare the stored strings, and
//! a space sorts before `T`, so a legacy row misorders against any row written
//! since. Postgres never stored the space form, so this only runs on `SQLite`.

use anyhow::{Context as _, Result};

use crate::{
    core::Registry,
    db::{
        DbConnection,
        migrate::helpers::{get_table_columns, table_exists},
        query::helpers::{global_table, versions_table},
    },
};

use super::meta;

/// Stored as the meta value; bump to force a re-run after a change here.
const MIGRATION_VERSION: &str = "1";

/// The gate of the pass this replaced, which covered the whole database at
/// once. Tables are never dropped, so a collection absent from the registry at
/// that run kept its space-form timestamps and — the gate being stamped — would
/// never have been rewritten once it was added back. Removed so no orphaned
/// gate outlives it.
const RETIRED_META_KEY: &str = "legacy_timestamps_normalized";

/// The job table, rewritten under a gate of its own.
const JOBS_TABLE: &str = "_crap_jobs";

/// The gate of one target, keyed by its table so a collection and a global of
/// the same slug can't share one.
fn meta_key(table: &str) -> String {
    format!("legacy_timestamps:{table}")
}

/// Every column that holds a system timestamp, on any table this touches.
/// Columns a table lacks are skipped.
const TIMESTAMP_COLUMNS: &[&str] = &[
    "created_at",
    "updated_at",
    "_deleted_at",
    "started_at",
    "completed_at",
    "heartbeat_at",
    "retry_after",
];

/// Rewrite legacy timestamps once per collection, global and the job table
/// (`SQLite` only).
///
/// The gate is per target, not per database: a table left in the database by a
/// collection absent from the registry is rewritten the first time that
/// collection is defined again, however long after the other tables were.
///
/// # Errors
///
/// Returns a backend error if introspection, an UPDATE, or the meta upsert
/// fails.
pub(super) fn normalize_if_needed(conn: &dyn DbConnection, registry: &Registry) -> Result<()> {
    if conn.is_postgres() {
        return Ok(());
    }

    // Deleting an absent key changes nothing, so this runs whether or not the
    // replaced pass ever did.
    meta::delete(conn, RETIRED_META_KEY)?;

    normalize_target(conn, JOBS_TABLE, &[JOBS_TABLE.to_string()])?;

    for slug in registry.collections.keys() {
        let tables = [slug.to_string(), versions_table(slug)];
        normalize_target(conn, slug, &tables)?;
    }

    for slug in registry.globals.keys() {
        let global = global_table(slug);
        let tables = [global.clone(), versions_table(&global)];
        normalize_target(conn, &global, &tables)?;
    }

    Ok(())
}

/// Rewrite the `tables` of one target, once per [`MIGRATION_VERSION`].
fn normalize_target(conn: &dyn DbConnection, gate: &str, tables: &[String]) -> Result<()> {
    let key = meta_key(gate);
    if meta::get(conn, &key)?.as_deref() == Some(MIGRATION_VERSION) {
        return Ok(());
    }

    for table in tables {
        normalize_table(conn, table)?;
    }

    meta::upsert(conn, &key, MIGRATION_VERSION)
}

fn normalize_table(conn: &dyn DbConnection, table: &str) -> Result<()> {
    if !table_exists(conn, table)? {
        return Ok(());
    }

    let columns = get_table_columns(conn, table)?;

    for column in TIMESTAMP_COLUMNS.iter().filter(|c| columns.contains(**c)) {
        conn.execute(
            &format!(
                "UPDATE \"{table}\" \
                 SET \"{column}\" = substr(\"{column}\", 1, 10) || 'T' || substr(\"{column}\", 12) || '.000Z' \
                 WHERE length(\"{column}\") = 19 AND substr(\"{column}\", 11, 1) = ' '"
            ),
            &[],
        )
        .with_context(|| format!("Failed to normalize legacy timestamps in {table}.{column}"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core::CollectionDefinition, db::InMemoryConn};

    fn registry_with(slugs: &[&str]) -> Registry {
        let shared = Registry::shared();

        for slug in slugs {
            shared
                .write()
                .unwrap()
                .register_collection(CollectionDefinition::new(*slug));
        }

        (*Registry::snapshot(&shared)).clone()
    }

    fn registry_with_posts() -> Registry {
        registry_with(&["posts"])
    }

    fn created_at(conn: &InMemoryConn, table: &str, id: &str) -> String {
        conn.0
            .query_row(
                &format!("SELECT created_at FROM {table} WHERE id = ?1"),
                [id],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// Legacy values are rewritten in place once; current values and other
    /// tables' columns are untouched, and the gate keeps a later run from
    /// scanning again.
    #[test]
    fn legacy_timestamps_are_rewritten_once() {
        let conn = InMemoryConn::open();
        conn.0
            .execute_batch(
                "CREATE TABLE _crap_meta (key TEXT PRIMARY KEY, value TEXT);
                 CREATE TABLE posts (id TEXT PRIMARY KEY, created_at TEXT, updated_at TEXT);
                 CREATE TABLE _crap_jobs (id TEXT PRIMARY KEY, created_at TEXT, completed_at TEXT);
                 INSERT INTO posts VALUES ('old', '2024-01-15 12:30:45', '2024-01-15 12:31:00');
                 INSERT INTO posts VALUES ('new', '2024-02-01T08:00:00.123Z', NULL);
                 INSERT INTO _crap_jobs VALUES ('j1', '2024-01-15 09:00:00', '2024-01-15 09:00:05');",
            )
            .unwrap();
        let registry = registry_with_posts();

        normalize_if_needed(&conn, &registry).unwrap();

        assert_eq!(
            created_at(&conn, "posts", "old"),
            "2024-01-15T12:30:45.000Z"
        );
        assert_eq!(
            created_at(&conn, "posts", "new"),
            "2024-02-01T08:00:00.123Z"
        );
        assert_eq!(
            created_at(&conn, "_crap_jobs", "j1"),
            "2024-01-15T09:00:00.000Z"
        );
        assert_eq!(
            meta::get(&conn, &meta_key("posts")).unwrap().as_deref(),
            Some(MIGRATION_VERSION)
        );

        conn.0
            .execute_batch("INSERT INTO posts VALUES ('later', '2024-03-01 00:00:00', NULL);")
            .unwrap();
        normalize_if_needed(&conn, &registry).unwrap();

        assert_eq!(
            created_at(&conn, "posts", "later"),
            "2024-03-01 00:00:00",
            "the gate must stop a second scan"
        );
    }

    /// Regression: one gate for the whole database stamped itself on the first
    /// boot, so a table left behind by a collection the registry didn't hold
    /// yet kept its space-form timestamps for good — misordering its lists and
    /// breaking its keyset cursors. Each target carries its own gate, so the
    /// collection is rewritten the first boot it is defined.
    #[test]
    fn a_collection_defined_later_is_still_rewritten() {
        let conn = InMemoryConn::open();
        conn.0
            .execute_batch(
                "CREATE TABLE _crap_meta (key TEXT PRIMARY KEY, value TEXT);
                 CREATE TABLE posts (id TEXT PRIMARY KEY, created_at TEXT);
                 CREATE TABLE pages (id TEXT PRIMARY KEY, created_at TEXT);
                 CREATE TABLE _crap_jobs (id TEXT PRIMARY KEY, created_at TEXT);
                 INSERT INTO posts VALUES ('p1', '2024-01-15 12:30:45');
                 INSERT INTO pages VALUES ('g1', '2024-01-16 08:00:00');",
            )
            .unwrap();

        // `pages` exists in the database but not in the registry — its Lua
        // definition is added after the first boot.
        normalize_if_needed(&conn, &registry_with(&["posts"])).unwrap();

        assert_eq!(created_at(&conn, "posts", "p1"), "2024-01-15T12:30:45.000Z");
        assert_eq!(created_at(&conn, "pages", "g1"), "2024-01-16 08:00:00");

        normalize_if_needed(&conn, &registry_with(&["posts", "pages"])).unwrap();

        assert_eq!(
            created_at(&conn, "pages", "g1"),
            "2024-01-16T08:00:00.000Z",
            "a collection defined after the first boot must still be rewritten"
        );
    }

    /// The gate of the pass this replaced is removed, so no orphaned
    /// whole-database flag outlives it.
    #[test]
    fn removes_the_replaced_passs_gate() {
        let conn = InMemoryConn::open();
        conn.0
            .execute_batch(
                "CREATE TABLE _crap_meta (key TEXT PRIMARY KEY, value TEXT);
                 CREATE TABLE _crap_jobs (id TEXT PRIMARY KEY, created_at TEXT);
                 INSERT INTO _crap_meta VALUES ('legacy_timestamps_normalized', '1');",
            )
            .unwrap();

        normalize_if_needed(&conn, &registry_with(&[])).unwrap();

        assert_eq!(meta::get(&conn, RETIRED_META_KEY).unwrap(), None);
    }
}
