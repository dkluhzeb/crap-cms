//! Dynamic schema migration: syncs SQLite tables to match Lua collection definitions.

mod backfill_ref_counts;
mod collection;
mod global;
pub mod helpers;
mod sync;
mod tracking;

pub use sync::sync_all;
pub use tracking::{
    drop_all_tables, get_applied_migrations, get_applied_migrations_desc, get_pending_migrations,
    list_migration_files, record_migration, remove_migration,
};
