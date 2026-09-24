//! Postgres harness: every row path — a row's own id, a group, a nested array
//! or blocks at any depth, a nested `_block_type` — matches the same documents
//! in SQL as in the in-memory evaluator.

#![cfg(all(test, feature = "postgres"))]

use super::{pg_test_pool, support::drop_tables_matching, unique_slug};
use crate::db::query::filter::row_paths_fixture::assert_row_paths_agree;

/// The Postgres JSON forms (`#>>`, `jsonb_array_elements_text` over a row's
/// column) read every row path as `SQLite` and the in-memory evaluator do.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_row_paths_agree_with_memory() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let conn = pool.get().expect("get PG connection");
    let slug = unique_slug("rowpaths");

    assert_row_paths_agree(&conn, &slug);

    drop_tables_matching(&conn, &slug);
}
