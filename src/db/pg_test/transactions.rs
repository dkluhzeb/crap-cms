//! Postgres harness: a failed statement aborts the whole transaction, and
//! `COMMIT` then answers `ROLLBACK` without an error. A commit must report
//! that, and a failed step inside a savepoint must leave the transaction
//! usable. A statement past its time limit is cancelled on the server.

#![cfg(all(test, feature = "postgres"))]

use std::time::{Duration, Instant};

use anyhow::Result;

use super::support::*;
use super::{pg_test_pool, unique_slug};
use crate::db::{
    DbConnection, DbValue, InPlaceTransaction, StatementDeadlineScope, StatementTimedOut,
    UnboundedStatements, with_savepoint,
};

fn create_table(conn: &dyn DbConnection, table: &str) {
    conn.execute(
        &format!("CREATE TABLE \"{table}\" (x BIGINT PRIMARY KEY)"),
        &[],
    )
    .expect("create table");
}

fn insert(conn: &dyn DbConnection, table: &str, x: i64) -> Result<usize> {
    conn.execute(
        &format!("INSERT INTO \"{table}\" (x) VALUES ($1)"),
        &[DbValue::Integer(x)],
    )
}

fn drop_table(conn: &dyn DbConnection, table: &str) {
    conn.execute(&format!("DROP TABLE \"{table}\""), &[])
        .expect("drop table");
}

/// Regression: a caller that caught a failed statement's error and carried
/// on was told its transaction committed — Postgres had rolled back every
/// write in it, and the driver read the `ROLLBACK` answer as success.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_committing_an_aborted_transaction_is_an_error() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let table = unique_slug("aborted_tx");
    create_table(&pool.get().expect("conn"), &table);

    {
        let mut conn = pool.write().expect("conn");
        let tx = conn.transaction_immediate().expect("begin");

        insert(&tx, &table, 1).expect("the first write succeeds");
        assert!(
            insert(&tx, &table, 1).is_err(),
            "a duplicate key fails and aborts the transaction"
        );

        let Err(e) = tx.commit() else {
            panic!("the commit of an aborted transaction must not report success");
        };
        assert!(format!("{e:#}").contains("rolled back"), "got: {e:#}");
    }

    let conn = pool.get().expect("conn");
    assert_eq!(row_count(&conn, &table), 0, "nothing was committed");
    drop_table(&conn, &table);
}

/// The same for a transaction opened in place (hook scopes, `crap.*` CRUD).
/// The connection is back in autocommit afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_committing_an_aborted_in_place_transaction_is_an_error() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let table = unique_slug("aborted_inplace");
    let conn = pool.get().expect("conn");
    create_table(&conn, &table);

    let tx = InPlaceTransaction::begin_on(&conn).expect("begin");
    insert(tx.conn(), &table, 1).expect("the first write succeeds");
    assert!(insert(tx.conn(), &table, 1).is_err());

    let Err(e) = tx.commit() else {
        panic!("the commit of an aborted transaction must not report success");
    };
    assert!(format!("{e:#}").contains("rolled back"), "got: {e:#}");
    assert!(!conn.in_transaction(), "the connection is settled");

    insert(&conn, &table, 2).expect("the connection is usable again");
    assert_eq!(row_count(&conn, &table), 1);
    drop_table(&conn, &table);
}

/// A failed step inside a savepoint is rolled back to its start and the
/// transaction goes on: the writes around it commit — the behaviour `SQLite`
/// has for a single failed statement, on both backends for a whole step.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_failed_step_keeps_the_transaction_usable() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let table = unique_slug("savepoint_step");
    create_table(&pool.get().expect("conn"), &table);

    {
        let tx = InPlaceTransaction::begin_owned(pool.write().expect("conn")).expect("begin");
        let conn = tx.conn();

        insert(conn, &table, 1).expect("before the step");

        let step = with_savepoint(conn, || -> Result<()> {
            insert(conn, &table, 2)?;
            insert(conn, &table, 1)?;
            Ok(())
        })
        .expect("the savepoint itself works");
        assert!(step.is_err(), "the step's duplicate key fails");

        insert(conn, &table, 3).expect("the transaction is still usable");
        tx.commit()
            .expect("a failure rolled back to its savepoint does not abort the commit");
    }

    let conn = pool.get().expect("conn");
    let rows = conn
        .query_all(&format!("SELECT x FROM \"{table}\" ORDER BY x"), &[])
        .expect("select");
    let values: Vec<i64> = rows.iter().filter_map(|r| r.i64_at(0)).collect();
    assert_eq!(values, vec![1, 3], "the step's own write was rolled back");
    drop_table(&conn, &table);
}

/// A step that swallows its own failed statement and reports success still
/// cannot leave the transaction aborted: the savepoint release fails, the
/// step is rolled back, and the error is reported.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_step_that_swallows_a_failure_is_rolled_back() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let table = unique_slug("swallowed_step");
    create_table(&pool.get().expect("conn"), &table);

    {
        let tx = InPlaceTransaction::begin_owned(pool.write().expect("conn")).expect("begin");
        let conn = tx.conn();

        insert(conn, &table, 1).expect("before the step");

        let step = with_savepoint(conn, || -> Result<()> {
            insert(conn, &table, 2)?;
            let _ = insert(conn, &table, 1);
            Ok(())
        });
        assert!(step.is_err(), "the unusable step is reported");

        insert(conn, &table, 3).expect("the transaction is still usable");
        tx.commit().expect("commit");
    }

    let conn = pool.get().expect("conn");
    assert_eq!(row_count(&conn, &table), 2);
    drop_table(&conn, &table);
}

/// Regression: a runaway query ran for as long as the server let it. Past
/// the operation's deadline it is cancelled on the server, fails with a
/// typed error, and the connection stays usable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_statement_past_its_deadline_is_cancelled() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let conn = pool.get().expect("conn");
    let started = Instant::now();

    let err = {
        let _scope =
            StatementDeadlineScope::bound_to(Some(Instant::now() + Duration::from_millis(100)));
        conn.query_one("SELECT 1 AS one FROM pg_sleep(30)", &[])
            .unwrap_err()
    };

    assert!(err.downcast_ref::<StatementTimedOut>().is_some(), "{err:#}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(conn.query_one("SELECT 1 AS one", &[]).unwrap().is_some());
}

/// Maintenance lifts the budget: a statement an expired deadline would
/// cancel runs to its end inside an unbounded scope.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_lifted_budget_lets_a_statement_run() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let conn = pool.get().expect("conn");
    let _scope = StatementDeadlineScope::bound_to(Some(Instant::now()));
    let _lift = UnboundedStatements::lift();

    assert!(
        conn.query_one("SELECT 1 AS one FROM pg_sleep(0.2)", &[])
            .unwrap()
            .is_some()
    );
}
