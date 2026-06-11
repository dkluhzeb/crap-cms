//! Dynamic schema migration: syncs `SQLite` tables to match Lua collection definitions.

mod backfill_ref_counts;
mod checkbox_columns;
#[cfg(not(test))]
mod collection;
#[cfg(test)]
pub(crate) mod collection;
mod global;
pub mod helpers;
mod sync;
mod tracking;

/// Test-only re-export so `test_helpers::setup_db` and the scheduler
/// tests can build the standard `_crap_jobs` schema via the
/// production migration path (single source of truth — schema can't
/// drift between test setup and prod).
#[cfg(test)]
pub(crate) use sync::create_jobs_table;
pub use sync::sync_all;
pub use tracking::{
    drop_all_tables, get_applied_migrations, get_applied_migrations_desc, get_pending_migrations,
    list_migration_files, record_migration, remove_migration,
};
