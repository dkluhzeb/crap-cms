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

const META_KEY: &str = "legacy_timestamps_normalized";

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

/// Rewrite legacy timestamps once per database (`SQLite` only).
///
/// # Errors
///
/// Returns a backend error if introspection, an UPDATE, or the meta upsert
/// fails.
pub(super) fn normalize_if_needed(conn: &dyn DbConnection, registry: &Registry) -> Result<()> {
    if conn.is_postgres() {
        return Ok(());
    }

    if meta::get(conn, META_KEY)?.as_deref() == Some(MIGRATION_VERSION) {
        return Ok(());
    }

    for table in candidate_tables(registry) {
        normalize_table(conn, &table)?;
    }

    meta::upsert(conn, META_KEY, MIGRATION_VERSION)
}

/// Collection, global and version tables, plus the job table.
fn candidate_tables(registry: &Registry) -> Vec<String> {
    let mut tables = vec!["_crap_jobs".to_string()];

    for slug in registry.collections.keys() {
        tables.push(slug.to_string());
        tables.push(versions_table(slug));
    }

    for slug in registry.globals.keys() {
        let global = global_table(slug);
        tables.push(versions_table(&global));
        tables.push(global);
    }

    tables
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

    fn registry_with_posts() -> Registry {
        let shared = Registry::shared();
        shared
            .write()
            .unwrap()
            .register_collection(CollectionDefinition::new("posts"));

        (*Registry::snapshot(&shared)).clone()
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
            meta::get(&conn, META_KEY).unwrap().as_deref(),
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
}
