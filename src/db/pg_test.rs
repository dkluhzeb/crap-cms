//! Postgres behavioral test harness.
//!
//! The unit suite runs on `SQLite` (`setup_db`/`setup_conn` create a temp
//! `SQLite` pool), so Postgres-only behavior — `NULL` sort ordering, JSON
//! path extraction, MVCC concurrency — was never exercised by a test.
//! This module is the seed of a dual-backend suite: it connects to a live
//! Postgres from `TEST_DATABASE_URL` and skips cleanly when that env var is
//! unset, so `cargo test` stays green without Postgres while
//! `TEST_DATABASE_URL=… cargo test --features postgres` runs the PG checks.
//!
//! Each test names its tables with a unique suffix (see [`unique_slug`]) so
//! parallel tests sharing one database don't collide.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::{CrapConfig, DbUrl};
use crate::db::DbPool;

/// Build a pool against the `TEST_DATABASE_URL` Postgres, or `None` when the
/// env var is unset (so the test skips rather than fails). Also returns `None`
/// if the pool cannot be built (bad URL) — the caller treats that as skip.
pub(crate) fn pg_test_pool() -> Option<DbPool> {
    let url = std::env::var("TEST_DATABASE_URL").ok()?;

    let mut config = CrapConfig::default();
    config.database.url = Some(DbUrl::from(url));

    crate::db::backend::postgres::create_pool(&config).ok()
}

/// A process-unique table/collection slug so parallel PG tests sharing one
/// database never collide on table names.
pub(crate) fn unique_slug(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{}_{n}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DbConnection, DbValue};

    /// Smoke test: prove the harness can connect to Postgres and round-trip a
    /// value through the `DbConnection` trait. Skips when `TEST_DATABASE_URL`
    /// is unset.
    ///
    /// PG tests run on a multi-threaded Tokio runtime: the Postgres backend
    /// bridges its async client to the sync `DbConnection` interface with
    /// `block_in_place`, which panics outside a multi-thread runtime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pg_harness_connects_and_round_trips() {
        let Some(pool) = pg_test_pool() else {
            eprintln!("skipping: TEST_DATABASE_URL not set");
            return;
        };

        let conn = pool.get().expect("get PG connection");
        let table = unique_slug("smoke");

        conn.execute(
            &format!("CREATE TABLE \"{table}\" (id TEXT PRIMARY KEY, n INTEGER)"),
            &[],
        )
        .unwrap();
        conn.execute(
            &format!("INSERT INTO \"{table}\" (id, n) VALUES ($1, $2)"),
            &[DbValue::Text("a".into()), DbValue::Integer(42)],
        )
        .unwrap();

        let row = conn
            .query_one(
                &format!("SELECT n FROM \"{table}\" WHERE id = $1"),
                &[DbValue::Text("a".into())],
            )
            .unwrap()
            .expect("row exists");
        assert_eq!(row.get_i64("n").unwrap(), 42);

        conn.execute(&format!("DROP TABLE \"{table}\""), &[])
            .unwrap();
    }

    /// Regression: a filter on a Group sub-field nested inside a Blocks/Array
    /// field reaches `json_extract_expr` with a DOTTED path (`meta.title`).
    /// Postgres `->>` takes a single key, so `->>'meta.title'` looks for a
    /// literal key named `meta.title` and returns NULL — the filter never
    /// matches. The fix uses the `#>>'{meta,title}'` path form.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pg_json_extract_handles_nested_dot_path() {
        let Some(pool) = pg_test_pool() else {
            eprintln!("skipping: TEST_DATABASE_URL not set");
            return;
        };

        let conn = pool.get().expect("get PG connection");
        let table = unique_slug("jsonpath");

        conn.execute(
            &format!("CREATE TABLE \"{table}\" (id TEXT, data JSONB)"),
            &[],
        )
        .unwrap();
        conn.execute(
            &format!(
                "INSERT INTO \"{table}\" (id, data) VALUES ('x', '{{\"meta\":{{\"title\":\"hello\"}}}}')"
            ),
            &[],
        )
        .unwrap();

        let extract = conn.json_extract_expr("data", "meta.title");
        let row = conn
            .query_one(
                &format!("SELECT id FROM \"{table}\" WHERE {extract} = $1"),
                &[DbValue::Text("hello".into())],
            )
            .unwrap();
        assert!(
            row.is_some(),
            "a nested dot-path filter must match on Postgres (extract expr: {extract})"
        );

        conn.execute(&format!("DROP TABLE \"{table}\""), &[])
            .unwrap();
    }
}
