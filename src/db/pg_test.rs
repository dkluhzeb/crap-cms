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

    /// Regression: a `Number` sub-field extracted from JSON is `text` on
    /// Postgres, so comparing it against a numeric operand (bound as float8)
    /// errors (`operator does not exist: text > double precision`) or compares
    /// lexically. `json_number_cast` wraps it so the comparison is numeric.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pg_json_number_subfield_compares_numerically() {
        let Some(pool) = pg_test_pool() else {
            eprintln!("skipping: TEST_DATABASE_URL not set");
            return;
        };

        let conn = pool.get().expect("get PG connection");
        let table = unique_slug("jsonnum");

        conn.execute(
            &format!("CREATE TABLE \"{table}\" (id TEXT, data JSONB)"),
            &[],
        )
        .unwrap();
        conn.execute(
            &format!(
                "INSERT INTO \"{table}\" (id, data) VALUES ('x', '{{\"meta\":{{\"price\":42}}}}')"
            ),
            &[],
        )
        .unwrap();

        let extract = conn.json_extract_expr("data", "meta.price");

        // Without the cast: text-vs-float8 comparison errors on Postgres.
        let raw = conn.query_one(
            &format!("SELECT id FROM \"{table}\" WHERE {extract} > $1"),
            &[DbValue::Real(9.0)],
        );
        assert!(
            raw.is_err(),
            "a text JSON extract compared to a numeric operand must error on PG (proves the bug)"
        );

        // With the cast: numeric comparison holds (42 > 9, and 42 = 42).
        let numeric = conn.json_number_cast(&extract);
        let gt = conn
            .query_one(
                &format!("SELECT id FROM \"{table}\" WHERE {numeric} > $1"),
                &[DbValue::Real(9.0)],
            )
            .unwrap();
        assert!(gt.is_some(), "42 > 9 must hold numerically after the cast");

        let eq = conn
            .query_one(
                &format!("SELECT id FROM \"{table}\" WHERE {numeric} = $1"),
                &[DbValue::Real(42.0)],
            )
            .unwrap();
        assert!(
            eq.is_some(),
            "numeric equality on a nested JSON Number must match"
        );

        conn.execute(&format!("DROP TABLE \"{table}\""), &[])
            .unwrap();
    }

    /// Regression (root cause of the keyset dup/drop bug): Postgres defaults to
    /// NULLs-LAST on ASC, the opposite of `SQLite` (NULLs-first) — and the keyset
    /// clause assumes `SQLite`'s placement. The sort builder now emits an explicit
    /// `NULLS FIRST` (ASC) / `NULLS LAST` (DESC), which PG must honor so its row
    /// order matches `SQLite`'s and the keyset stays correct. This pins that PG
    /// honors the clause (its default would order NULLs the other way).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pg_null_sort_places_nulls_per_explicit_clause() {
        let Some(pool) = pg_test_pool() else {
            eprintln!("skipping: TEST_DATABASE_URL not set");
            return;
        };

        let conn = pool.get().expect("get PG connection");
        let table = unique_slug("nullsort");

        conn.execute(
            &format!("CREATE TABLE \"{table}\" (id TEXT, sort_val INTEGER)"),
            &[],
        )
        .unwrap();
        for (id, val) in [("a", "NULL"), ("b", "5"), ("c", "NULL"), ("d", "3")] {
            conn.execute(
                &format!("INSERT INTO \"{table}\" (id, sort_val) VALUES ('{id}', {val})"),
                &[],
            )
            .unwrap();
        }

        let ids = |sql: String| -> Vec<String> {
            conn.query_all(&sql, &[])
                .unwrap()
                .iter()
                .map(|r| r.get_string("id").unwrap())
                .collect()
        };

        // PG's DEFAULT (no NULLS clause) on ASC puts NULLs LAST — the divergence
        // from SQLite (NULLs-first) that broke keyset pagination.
        assert_eq!(
            ids(format!(
                "SELECT id FROM \"{table}\" ORDER BY sort_val ASC, id ASC"
            )),
            vec!["d", "b", "a", "c"],
            "PG default ASC orders NULLs last (this is the SQLite divergence)"
        );

        // The explicit clause the sort builder now emits flips PG to NULLs-first
        // on ASC (matching SQLite) and NULLs-last on DESC.
        assert_eq!(
            ids(format!(
                "SELECT id FROM \"{table}\" ORDER BY sort_val ASC NULLS FIRST, id ASC"
            )),
            vec!["a", "c", "d", "b"],
            "explicit NULLS FIRST matches SQLite's ASC default"
        );
        assert_eq!(
            ids(format!(
                "SELECT id FROM \"{table}\" ORDER BY sort_val DESC NULLS LAST, id ASC"
            )),
            vec!["b", "d", "a", "c"],
            "explicit NULLS LAST matches SQLite's DESC default"
        );

        conn.execute(&format!("DROP TABLE \"{table}\""), &[])
            .unwrap();
    }

    /// Regression: the ref-count update path snapshots outgoing refs with an
    /// UNLOCKED read before the document row is write-locked, so two concurrent
    /// updates to one document could both read a stale `old_refs` under Postgres
    /// MVCC and double-apply a delta (delete-protection bypass / phantom ref).
    /// The fix locks the row first via `lock_row` (SELECT … FOR UPDATE). This
    /// proves that lock is real and row-scoped: while one tx holds it, a
    /// concurrent `FOR UPDATE NOWAIT` fails on that row but succeeds on another.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pg_lock_row_holds_a_real_row_lock() {
        let Some(pool) = pg_test_pool() else {
            eprintln!("skipping: TEST_DATABASE_URL not set");
            return;
        };

        let table = unique_slug("lockrow");
        {
            let setup = pool.get().expect("get PG connection");
            setup
                .execute(
                    &format!("CREATE TABLE \"{table}\" (id TEXT PRIMARY KEY)"),
                    &[],
                )
                .unwrap();
            setup
                .execute(
                    &format!("INSERT INTO \"{table}\" (id) VALUES ('x'), ('y')"),
                    &[],
                )
                .unwrap();
        }

        let mut conn1 = pool.get().expect("conn1");
        let mut conn2 = pool.get().expect("conn2");

        let tx1 = conn1.transaction_immediate().unwrap();
        tx1.lock_row(&table, "x").unwrap();

        let tx2 = conn2.transaction_immediate().unwrap();

        // A row tx1 did NOT lock is still freely lockable (row-scoped).
        let free = tx2.query_one(
            &format!("SELECT 1 FROM \"{table}\" WHERE id='y' FOR UPDATE NOWAIT"),
            &[],
        );
        assert!(free.is_ok(), "an unlocked row must be lockable by tx2");

        // The row tx1 holds via lock_row cannot be locked concurrently.
        let locked = tx2.query_one(
            &format!("SELECT 1 FROM \"{table}\" WHERE id='x' FOR UPDATE NOWAIT"),
            &[],
        );
        assert!(
            locked.is_err(),
            "row 'x' held by tx1's lock_row must block a concurrent FOR UPDATE"
        );

        drop(tx2);
        drop(tx1);

        let cleanup = pool.get().expect("cleanup conn");
        cleanup
            .execute(&format!("DROP TABLE \"{table}\""), &[])
            .unwrap();
    }
}
