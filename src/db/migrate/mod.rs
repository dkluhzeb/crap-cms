//! Dynamic schema migration: syncs `SQLite` tables to match Lua collection definitions.

mod backfill_ref_counts;
mod canonical_text;
mod checkbox_columns;
#[cfg(not(test))]
mod collection;
#[cfg(test)]
pub(crate) mod collection;
mod global;
mod has_many_lists;
pub mod helpers;
mod identifier_check;
mod inline_unique;
mod legacy_timestamps;
mod locale_change;
mod meta;
mod nested_values;
mod nullable_columns;
mod one_time;
mod orphan_tables;
mod reference_cardinality;
mod relationship_target;
mod search_index;
mod sync;
mod tracking;

pub(crate) use backfill_ref_counts::recompute_ref_counts;
pub use locale_change::warn_on_default_locale_change;
pub use orphan_tables::{OrphanKind, OrphanTable, find_orphan_tables};
/// Test-only re-export so `test_helpers::setup_db` and the scheduler
/// tests can build the standard `_crap_jobs` schema via the
/// production migration path (single source of truth — schema can't
/// drift between test setup and prod).
#[cfg(test)]
pub(crate) use sync::create_jobs_table;
pub use sync::{check_all_identifiers, recreate_all, sync_all};
pub use tracking::{
    get_applied_migrations, get_applied_migrations_desc, get_pending_migrations,
    list_migration_files, record_migration, remove_migration,
};
