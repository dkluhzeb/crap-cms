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
use crate::db::{DbPool, backend::postgres::create_pool};

/// Build a pool against the `TEST_DATABASE_URL` Postgres, or `None` when the
/// env var is unset (so the test skips rather than fails). Also returns `None`
/// if the pool cannot be built (bad URL) — the caller treats that as skip.
pub(crate) fn pg_test_pool() -> Option<DbPool> {
    pg_test_pool_sized(CrapConfig::default().database.pool_max_size, None)
}

/// A pool of a chosen size, and optionally a chosen checkout timeout — for the
/// tests that need to exhaust it or prove a wait is bounded. `None` when
/// `TEST_DATABASE_URL` is unset, exactly like [`pg_test_pool`].
pub(crate) fn pg_test_pool_sized(max_size: u32, timeout_secs: Option<u64>) -> Option<DbPool> {
    let url = std::env::var("TEST_DATABASE_URL").ok()?;

    pg_pool_for(url, max_size, timeout_secs)
}

/// A pool against an arbitrary URL — for the connect failures that need no
/// reachable server at all.
pub(crate) fn pg_pool_for(url: String, max_size: u32, timeout_secs: Option<u64>) -> Option<DbPool> {
    let mut config = CrapConfig::default();
    config.database.url = Some(DbUrl::from(url));
    config.database.pool_max_size = max_size;

    if let Some(secs) = timeout_secs {
        config.database.connection_timeout = secs;
    }

    create_pool(&config).ok()
}

/// A process-unique table/collection slug so parallel PG tests sharing one
/// database never collide on table names.
pub(crate) fn unique_slug(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{}_{n}", std::process::id())
}

mod cache_pool;
mod draft_parents;
mod errors_sql_cron;
mod list_filters;
mod localized_join_filters;
mod queries;
mod row_paths;
mod soft_delete;
mod support;
