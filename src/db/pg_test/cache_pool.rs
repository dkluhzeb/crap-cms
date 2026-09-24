//! Postgres harness: the prepared-statement cache and the pool's timeouts,
//! recycling and connect-failure classification.

#![cfg(all(test, feature = "postgres"))]

use std::time::{Duration, Instant};

use tokio::time::sleep;

use super::support::*;
use super::{pg_pool_for, pg_test_pool, pg_test_pool_sized, unique_slug};
use crate::{
    db::{DbConnection, DbValue, is_transient},
    service::ServiceError,
};

/// A schema change under a cached statement must not break that statement
/// for the life of the connection.
///
/// Postgres plans a prepared statement once; an `ALTER TABLE` from another
/// node's schema sync, or a migration run against a live server, then makes
/// the plan fail with 0A000 "cached plan must not change result type". The
/// statement is re-prepared once and the query succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_cached_statement_survives_a_concurrent_alter_table() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let table = unique_slug("cachedplan");
    let conn = pool.get().expect("conn");
    conn.execute(
        &format!("CREATE TABLE \"{table}\" (id TEXT PRIMARY KEY, n INTEGER)"),
        &[],
    )
    .unwrap();
    conn.execute(
        &format!("INSERT INTO \"{table}\" (id, n) VALUES ('a', 1)"),
        &[],
    )
    .unwrap();

    // `SELECT *` is the shape whose *result type* an ALTER changes, which
    // is what invalidates the cached plan rather than merely the statistics.
    let select = format!("SELECT * FROM \"{table}\" WHERE id = $1");
    let first = conn
        .query_one(&select, &[DbValue::Text("a".into())])
        .expect("first read caches the statement");
    assert!(first.is_some());

    // A second connection performs the schema change, so the cache on the
    // first one is not invalidated locally.
    {
        let other = pool.get().expect("second conn");
        other
            .execute(
                &format!("ALTER TABLE \"{table}\" ADD COLUMN extra TEXT"),
                &[],
            )
            .unwrap();
    }

    let after = conn
        .query_one(&select, &[DbValue::Text("a".into())])
        .expect("the cached statement must be re-prepared, not fail forever");
    assert!(after.is_some(), "the row is still there after the ALTER");

    conn.execute(&format!("DROP TABLE \"{table}\""), &[])
        .unwrap();
}

/// A named prepared statement outlives the transaction that parsed it —
/// a rollback discards portals, not statements — so the cache keeps it and
/// the next use on the connection is served without a second `Parse`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_statement_prepared_in_a_rolled_back_transaction_is_reused() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let table = unique_slug("rollbackstmt");
    let mut conn = pool.get().expect("conn");
    conn.execute(
        &format!("CREATE TABLE \"{table}\" (id TEXT PRIMARY KEY)"),
        &[],
    )
    .unwrap();

    // A SQL string this connection has never seen, first parsed inside a
    // transaction that is then rolled back (dropped without commit).
    let select = format!("SELECT id FROM \"{table}\" WHERE id = $1 AND id IS NOT NULL");
    let before = prepared_statement_count(&conn);
    {
        let tx = conn.transaction_immediate().expect("tx");
        tx.query_one(&select, &[DbValue::Text("a".into())]).unwrap();
    }

    assert_eq!(
        prepared_statement_count(&conn),
        before + 1,
        "the server keeps a statement parsed in a rolled-back transaction"
    );

    conn.query_one(&select, &[DbValue::Text("a".into())])
        .expect("the cached statement is still valid after the rollback");
    assert_eq!(
        prepared_statement_count(&conn),
        before + 1,
        "the second use must be served from the cache, not re-prepared"
    );

    conn.execute(&format!("DROP TABLE \"{table}\""), &[])
        .unwrap();
}

/// A connect the server refused outright carries no SQLSTATE; it is the
/// caller's cue to retry, like a lost connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_refused_connection_reads_as_transient() {
    let pool = pg_pool_for("postgres://nobody@127.0.0.1:1/nothing".into(), 1, Some(2))
        .expect("a pool builds without connecting");

    let err = pool.get().err().expect("nothing listens on port 1");

    assert!(
        is_transient(&err),
        "refused connect must be transient: {err:#}"
    );
}

/// A rejected password is a configuration error: retrying cannot help,
/// so it must not be reported as a transient failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_rejected_password_is_not_transient() {
    let Some(url) = std::env::var("TEST_DATABASE_URL")
        .ok()
        .and_then(|url| with_wrong_password(&url))
    else {
        eprintln!("skipping: TEST_DATABASE_URL not set or carries no password");
        return;
    };
    let pool = pg_pool_for(url, 1, Some(5)).expect("a pool builds without connecting");

    let err = pool.get().err().expect("the wrong password is rejected");

    assert!(
        !is_transient(&err),
        "an auth failure is not transient: {err:#}"
    );
}

/// An exhausted pool fails the checkout instead of waiting forever.
///
/// `PoolBackend::get` blocks a Tokio worker thread on the checkout future,
/// so an unbounded wait parks that worker for as long as the pool stays
/// exhausted. The error is classified transient, the same as the `SQLite`
/// pool's r2d2 timeout, so every surface answers 503 rather than 500.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_an_exhausted_pool_times_out_and_reads_as_transient() {
    let Some(pool) = pg_test_pool_sized(1, Some(1)) else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let _held = pool.get().expect("the pool's only connection");

    let started = Instant::now();
    let err = pool
        .get()
        .err()
        .expect("a pool of one with its connection held must not hand out a second");

    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the checkout must give up at the configured timeout, not hang"
    );
    assert!(
        matches!(
            ServiceError::classify(err, "postgres"),
            ServiceError::Transient(_)
        ),
        "an exhausted pool is a retryable condition"
    );
}

/// Regression: every boot logged a page of `tokio_postgres` INFO lines —
/// one NOTICE ("relation … already exists, skipping") per `CREATE … IF NOT
/// EXISTS` the schema sync runs. Pooled sessions only receive warnings and
/// errors, so the server never sends those notices.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_sessions_receive_no_notices() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let conn = pool.get().expect("conn");
    let row = conn
        .query_one(
            "SELECT current_setting('client_min_messages') AS level",
            &[],
        )
        .unwrap()
        .unwrap();

    assert_eq!(row.get_string("level").unwrap(), "warning");
}

/// A connection killed server-side must leave the pool rather than be
/// handed out again. Recycling accepted every pooled client unconditionally,
/// so after a server restart every checkout of a dead client failed with
/// "connection closed" for the life of the process.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_a_killed_connection_is_recycled_out_of_the_pool() {
    let Some(pool) = pg_test_pool_sized(1, Some(5)) else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    {
        let victim = pool.get().expect("the pool's only connection");
        let pid = victim
            .query_one("SELECT pg_backend_pid() AS pid", &[])
            .unwrap()
            .unwrap()
            .get_i64("pid")
            .unwrap();

        // Kill it from a connection outside this pool, then return the dead
        // client so recycling has to notice.
        let killer = pg_test_pool_sized(1, Some(5)).expect("killer pool");
        let killer_conn = killer.get().expect("killer conn");
        killer_conn
            .execute("SELECT pg_terminate_backend($1)", &[DbValue::Integer(pid)])
            .unwrap();
    }

    // Give the driver a moment to observe the closed socket.
    sleep(Duration::from_millis(250)).await;

    let conn = pool
        .get()
        .expect("the pool must hand out a live connection");
    let row = conn
        .query_one("SELECT 1 AS ok", &[])
        .expect("usable connection");
    assert_eq!(row.unwrap().get_i64("ok").unwrap(), 1);
}
