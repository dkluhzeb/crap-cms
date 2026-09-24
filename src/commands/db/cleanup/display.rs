//! Terminal output for `db cleanup`: what a scan found, and what a committed
//! cleanup changed.

use super::{apply::CleanupOutcome, scan::CleanupReport};
use crate::{cli, db::migrate::OrphanTable};

/// Display the tables no definition accounts for.
fn display_orphan_tables(tables: &[OrphanTable]) {
    cli::warning("Tables not in any Lua definition:");
    println!();

    for table in tables {
        cli::dim(&format!("  {} ({})", table.name, table.kind.label()));
    }

    println!();
    cli::info(&format!("{} orphan table(s) found.", tables.len()));
}

/// Display the list of orphan columns found.
fn display_orphans(orphans: &[(String, Vec<String>)]) {
    cli::warning("Orphan columns (not in Lua definitions):");
    println!();

    for (table, cols) in orphans {
        for col in cols {
            cli::dim(&format!("  {table}.{col}"));
        }
    }

    let total: usize = orphans.iter().map(|(_, cols)| cols.len()).sum();

    println!();
    cli::info(&format!("{total} orphan column(s) found."));
}

/// Display the junction rows whose locale is no longer configured.
fn display_stale_locale_rows(stale: &[(String, i64)]) {
    cli::warning("Rows of locales the project no longer configures:");
    println!();

    for (table, count) in stale {
        cli::dim(&format!("  {table}: {count} row(s)"));
    }

    let total: i64 = stale.iter().map(|(_, count)| *count).sum();

    println!();
    cli::info(&format!("{total} stale locale row(s) found."));
}

/// Display what the scan found.
pub(super) fn display_report(report: &CleanupReport) {
    if !report.columns.is_empty() {
        display_orphans(&report.columns);
    }

    if !report.stale_locale_rows.is_empty() {
        display_stale_locale_rows(&report.stale_locale_rows);
    }

    if !report.tables.is_empty() {
        display_orphan_tables(&report.tables);
    }
}

/// Display what a committed cleanup changed. Only ever called after the
/// commit, so every line describes a change that is actually stored.
pub(super) fn display_outcome(outcome: &CleanupOutcome) {
    if !outcome.dropped_columns.is_empty() {
        for column in &outcome.dropped_columns {
            cli::success(&format!("Dropped: {column}"));
        }

        cli::success(&format!(
            "{} column(s) dropped.",
            outcome.dropped_columns.len()
        ));
    }

    if !outcome.deleted_rows.is_empty() {
        for (table, deleted) in &outcome.deleted_rows {
            cli::success(&format!("Deleted: {deleted} row(s) from {table}"));
        }

        let total: usize = outcome.deleted_rows.iter().map(|(_, n)| *n).sum();
        cli::success(&format!("{total} stale locale row(s) deleted."));
    }

    if !outcome.dropped_tables.is_empty() {
        for table in &outcome.dropped_tables {
            cli::success(&format!("Dropped: {table}"));
        }

        cli::success(&format!(
            "{} table(s) dropped.",
            outcome.dropped_tables.len()
        ));
    }
}
