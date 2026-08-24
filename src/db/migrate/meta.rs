//! Shared `_crap_meta` key/value accessors for versioned one-time migrations.
//!
//! One-time migrations (the ref-count backfill, the checkbox retype) gate
//! themselves on a version stored as the meta *value* under a stable key — see
//! [`super::backfill_ref_counts`] and [`super::checkbox_columns`]. Each reads the
//! current value to decide whether to run and, on completion, writes the current
//! version back. This module owns that read plus the DELETE-then-INSERT upsert
//! (backend-agnostic — it assumes neither `SQLite` nor Postgres `ON CONFLICT`),
//! so the two gates can never drift on the "replace a stale value cleanly"
//! contract. Every future one-time migration reuses these instead of re-copying
//! the SQL.

use anyhow::Result;

use crate::db::{DbConnection, DbValue};

/// Read a `_crap_meta` value by key. `None` when the key is absent.
pub(super) fn get(conn: &dyn DbConnection, key: &str) -> Result<Option<String>> {
    let p1 = conn.placeholder(1);
    let row = conn.query_one(
        &format!("SELECT value FROM _crap_meta WHERE key = {p1}"),
        &[DbValue::Text(key.to_string())],
    )?;

    Ok(row.and_then(|r| r.text_at(0).map(str::to_string)))
}

/// Upsert a `_crap_meta` key via DELETE + INSERT (backend-agnostic), so a stale
/// value from an earlier run is replaced cleanly rather than left behind or
/// duplicated.
pub(super) fn upsert(conn: &dyn DbConnection, key: &str, value: &str) -> Result<()> {
    let p1 = conn.placeholder(1);
    conn.execute(
        &format!("DELETE FROM _crap_meta WHERE key = {p1}"),
        &[DbValue::Text(key.to_string())],
    )?;

    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
    conn.execute(
        &format!("INSERT INTO _crap_meta (key, value) VALUES ({p1}, {p2})"),
        &[
            DbValue::Text(key.to_string()),
            DbValue::Text(value.to_string()),
        ],
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CrapConfig, DatabaseConfig, LocaleConfig};
    use crate::core::Registry;
    use crate::db::{DbPool, migrate, pool};

    fn setup_db() -> (tempfile::TempDir, DbPool) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..CrapConfig::test_default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

        let registry = Registry::new();
        migrate::sync_all(&db_pool, &registry, &LocaleConfig::default()).expect("sync");

        (tmp, db_pool)
    }

    #[test]
    fn get_returns_none_for_absent_key() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();

        assert_eq!(get(&conn, "does_not_exist").unwrap(), None);
    }

    #[test]
    fn upsert_then_get_round_trips() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();

        upsert(&conn, "k", "1").unwrap();
        assert_eq!(get(&conn, "k").unwrap().as_deref(), Some("1"));
    }

    /// The reason this is a shared chokepoint: upsert must REPLACE a stale value
    /// (not append a second row or leave the old one), so a version bump on an
    /// existing database takes effect. A plain INSERT would violate the PK or
    /// leave the read ambiguous.
    #[test]
    fn upsert_replaces_stale_value() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();

        upsert(&conn, "version_gate", "1").unwrap();
        upsert(&conn, "version_gate", "2").unwrap();

        assert_eq!(get(&conn, "version_gate").unwrap().as_deref(), Some("2"));

        // Exactly one row survives — the replace didn't duplicate.
        let rows = conn
            .query_all(
                &format!(
                    "SELECT value FROM _crap_meta WHERE key = {}",
                    conn.placeholder(1)
                ),
                &[DbValue::Text("version_gate".to_string())],
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
    }
}
