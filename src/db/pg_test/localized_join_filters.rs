//! Postgres harness: filters on localized join rows — a has-many junction and
//! an array with a `_locale` column — match exactly the rows the read shows,
//! the fallback locale's included.

#![cfg(all(test, feature = "postgres"))]

use super::{pg_test_pool, support::drop_tables_matching, unique_slug};
use crate::db::query::filter::localized_rows_fixture::assert_filters_match_the_shown_rows;

/// Every operator on a localized junction or array matches the documents
/// whose shown rows satisfy it — under fallback, without it, and for an
/// all-locales read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_localized_row_filters_match_the_rows_the_read_shows() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let conn = pool.get().expect("get PG connection");
    let slug = unique_slug("locrows");

    assert_filters_match_the_shown_rows(&conn, &slug);

    drop_tables_matching(&conn, &slug);
}
