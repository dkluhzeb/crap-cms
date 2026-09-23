//! Migration status shared by the `status` overview and `status --check`.

use std::path::Path;

use anyhow::Result;

use crate::db::{DbPool, migrate};

/// The migrations of a project: how many files exist on disk, how many are
/// recorded as applied, and which files are still pending.
pub(super) struct MigrationStatus {
    pub total: usize,
    pub applied: usize,
    pub pending: Vec<String>,
}

/// Read the migration status of the project at `config_dir`.
///
/// # Errors
///
/// Returns an error if the migrations directory can't be listed or the
/// applied set can't be read.
pub(super) fn migration_status(config_dir: &Path, pool: &DbPool) -> Result<MigrationStatus> {
    let migrations_dir = config_dir.join("migrations");
    let total = migrate::list_migration_files(&migrations_dir)?.len();
    let applied = migrate::get_applied_migrations(pool)?.len();
    let pending = migrate::get_pending_migrations(pool, &migrations_dir)?;

    Ok(MigrationStatus {
        total,
        applied,
        pending,
    })
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::fs;

    use super::*;
    use crate::{config::CrapConfig, db::pool};

    #[test]
    fn counts_files_and_lists_the_unapplied_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let migrations_dir = tmp.path().join("migrations");
        fs::create_dir_all(&migrations_dir).unwrap();
        fs::write(migrations_dir.join("20260101_000000_a.lua"), "return {}").unwrap();
        fs::write(migrations_dir.join("20260102_000000_b.lua"), "return {}").unwrap();

        let db_pool = pool::create_pool(tmp.path(), &CrapConfig::test_default()).unwrap();
        let status = migration_status(tmp.path(), &db_pool).unwrap();

        assert_eq!(status.total, 2);
        assert_eq!(status.applied, 0);
        assert_eq!(status.pending.len(), 2);
    }
}
