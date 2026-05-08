//! Shared test fixtures for jobs/ submodules.

use tempfile::TempDir;

use crate::config::CrapConfig;
use crate::db::{BoxedConnection, DbConnection, pool};

pub(super) fn setup_db() -> (TempDir, BoxedConnection) {
    let dir = TempDir::new().unwrap();
    let config = CrapConfig::default();
    let p = pool::create_pool(dir.path(), &config).unwrap();
    let conn = p.get().unwrap();
    conn.execute_batch(
        "CREATE TABLE _crap_jobs (
            id TEXT PRIMARY KEY,
            slug TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            queue TEXT NOT NULL DEFAULT 'default',
            data TEXT DEFAULT '{}',
            result TEXT,
            error TEXT,
            attempt INTEGER NOT NULL DEFAULT 0,
            max_attempts INTEGER NOT NULL DEFAULT 1,
            scheduled_by TEXT,
            created_at TEXT DEFAULT (datetime('now')),
            started_at TEXT,
            completed_at TEXT,
            heartbeat_at TEXT,
            retry_after TEXT
        );
        CREATE INDEX idx_crap_jobs_status ON _crap_jobs(status);
        CREATE INDEX idx_crap_jobs_queue ON _crap_jobs(queue, status);
        CREATE INDEX idx_crap_jobs_slug ON _crap_jobs(slug, status);",
    )
    .unwrap();
    (dir, conn)
}
