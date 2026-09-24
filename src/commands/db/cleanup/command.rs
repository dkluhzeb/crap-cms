//! The `db cleanup` command entry point.

use std::path::Path;

use anyhow::{Context as _, Result};

use super::{
    apply::apply_in_transaction,
    display::{display_outcome, display_report},
    scan::scan,
};
use crate::{
    cli,
    commands::{Project, open_project},
};

/// Detect and optionally remove leftovers no Lua definition accounts for.
///
/// Three kinds:
///
/// - **Orphan columns** — columns in a collection, global or junction table
///   that no field in the current Lua definition maps to. System columns
///   (`_`-prefixed, `id`, the timestamps) are always kept. Because Lua
///   definitions include plugin-added fields (plugins run during `init_lua`),
///   plugin columns are never flagged as orphans.
/// - **Stale locale rows** — junction rows (array, blocks, has-many
///   relationship) whose `_locale` names a locale the project no longer
///   configures. Dropping a locale leaves them unreachable but stored.
/// - **Orphan tables** — a whole collection, global, versions or junction
///   table whose definition is gone (a renamed or removed collection). These
///   are reported by default and only ever dropped when `drop_tables` is set
///   as well as `confirm`: a table nothing references can still hold the only
///   copy of its data.
///
/// By default runs in dry-run mode (report only). Pass `confirm = true` to
/// actually drop the columns and delete the rows. Every change is applied in
/// one transaction and reported only after it commits.
///
/// # Errors
///
/// Returns an error if config loading, Lua init, pool creation, schema
/// inspection, column drops, row deletes, or table drops fail.
#[cfg(not(tarpaulin_include))]
pub fn cleanup(config_dir: &Path, confirm: bool, drop_tables: bool) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let Project {
        lock: _instance_lock,
        config: cfg,
        registry,
        pool,
    } = open_project(&config_dir)?;

    let mut conn = pool.write().context("Failed to get database connection")?;
    let mut report = scan(&conn, &registry, &cfg.locale)?;

    if report.is_empty() {
        cli::success("Nothing to clean up. The schema matches the Lua definitions.");
        return Ok(());
    }

    display_report(&report);

    if !report.tables.is_empty() && !drop_tables {
        cli::hint("Tables are never dropped implicitly. Pass --drop-tables -y to remove them.");
    }

    if !confirm {
        cli::hint("This is a dry run. Pass --confirm to apply these changes.");
        cli::hint("Note: dropping columns and rows is irreversible. Back up your database first.");
        return Ok(());
    }

    // Tables are only ever dropped when asked for explicitly.
    if !drop_tables {
        report.tables.clear();
    }

    let outcome = apply_in_transaction(&mut conn, &report, &registry, &cfg.locale)?;

    display_outcome(&outcome);

    Ok(())
}
