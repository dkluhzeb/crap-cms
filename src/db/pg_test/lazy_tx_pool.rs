//! Postgres harness: a lazily opened hook transaction given the request's
//! read connection writes on it instead of checking out a second connection
//! from the one shared pool.

#![cfg(all(test, feature = "postgres"))]

use super::support::drop_tables_matching;
use super::{pg_test_pool_sized, unique_slug};
use crate::{db::DbConnection, hooks::lifecycle::LazyTx};

/// Regression: a per-request auth strategy that writes held the request's
/// read connection and waited for a write connection from the same pool —
/// so as many such requests as the pool has connections held all of them
/// and each timed out waiting for another. On a one-connection pool the
/// write now succeeds on the reader.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_lazy_tx_writes_on_the_reader_of_a_shared_pool() {
    let Some(pool) = pg_test_pool_sized(1, Some(2)) else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let table = unique_slug("lazytx");
    let reader = pool.get().expect("the only connection");
    reader
        .execute_batch(&format!("CREATE TABLE \"{table}\" (x INTEGER)"))
        .expect("create");

    let tx = LazyTx::on_pool_reading(pool.clone(), &reader, "test");
    let wrote = tx
        .conn()
        .and_then(|conn| conn.execute(&format!("INSERT INTO \"{table}\" VALUES (1)"), &[]));
    let committed = match wrote {
        Ok(_) => tx.commit(),
        Err(e) => {
            drop(tx);
            Err(e)
        }
    };

    let rows = reader
        .query_one(&format!("SELECT COUNT(*) AS c FROM \"{table}\""), &[])
        .ok()
        .flatten()
        .and_then(|row| row.get_i64("c").ok());

    drop_tables_matching(&reader, &table);

    committed.expect("the write opens on the reader, not a second connection");
    assert_eq!(rows, Some(1));
}
