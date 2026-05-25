//! Shared test fixtures for jobs/ submodules.

use tempfile::TempDir;

use crate::config::CrapConfig;
use crate::db::{BoxedConnection, pool};

pub(super) fn setup_db() -> (TempDir, BoxedConnection) {
    let dir = TempDir::new().unwrap();
    let config = CrapConfig::default();
    let p = pool::create_pool(dir.path(), &config).unwrap();
    let conn = p.get().unwrap();
    // Call the canonical migration so the schema can't drift from
    // production. `SQLite`-flavoured timestamp types are passed
    // through; the migration's idempotent `ALTER ADD COLUMN` blocks
    // are no-ops on a freshly-created table.
    crate::db::migrate::create_jobs_table(&conn, "TEXT DEFAULT (datetime('now'))", "TEXT")
        .expect("create_jobs_table");
    (dir, conn)
}
