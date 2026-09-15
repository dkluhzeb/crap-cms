//! Test fixtures shared by the runner submodules' unit tests.

use std::sync::Arc;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::OpenFlags;

use crate::{
    core::{JobDefinition, Registry, upload::ImageConvertJobData},
    db::{DbConnection, DbPool, migrate},
};

/// A registry snapshot holding exactly `jobs`.
pub(super) fn make_registry_with_jobs(jobs: Vec<JobDefinition>) -> Arc<Registry> {
    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        for job in jobs {
            reg.register_job(job);
        }
    }
    Registry::snapshot(&shared)
}

/// An in-memory `SQLite` pool with the jobs schema and `_crap_cron_fired`.
pub(super) fn make_test_pool() -> DbPool {
    let manager = SqliteConnectionManager::memory().with_flags(
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_SHARED_CACHE,
    );
    let inner = Pool::builder()
        .max_size(2)
        .test_on_check_out(true)
        .build(manager)
        .expect("Failed to create test pool");

    let pool = DbPool::from_pool(inner);

    // Build the standard jobs schema via the canonical migration
    // path so we can't drift from production. `_crap_cron_fired`
    // is colocated here since these tests exercise the cron loop.
    let conn = pool.get().unwrap();
    migrate::create_jobs_table(
        &conn,
        "TEXT DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        "TEXT",
    )
    .expect("create_jobs_table");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _crap_cron_fired (
            slug TEXT PRIMARY KEY,
            fired_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
        );",
    )
    .unwrap();
    drop(conn);

    pool
}

/// A queued image conversion of `collection/document_id`.
pub(super) fn convert_job(collection: &str, document_id: &str) -> ImageConvertJobData {
    ImageConvertJobData {
        collection: collection.into(),
        document_id: document_id.into(),
        source_path: "a.png".into(),
        target_path: "a.webp".into(),
        format: "webp".into(),
        quality: 80,
        url_column: "thumbnail_webp_url".into(),
        url_value: "/uploads/a.webp".into(),
    }
}
