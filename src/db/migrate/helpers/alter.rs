//! The single `ALTER TABLE … ADD COLUMN` chokepoint for reconcile paths.

use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result, bail};
use tracing::{info, warn};

use crate::db::DbConnection;
use crate::db::query::helpers::quote_ident;

/// Reconcile an existing scalar `has_many` column (a JSON array stored in TEXT)
/// whose physical type drifted to numeric on an older Postgres database created
/// before `ColumnSpec::ddl_type` routed it to TEXT. Writing the JSON-array
/// string into a numeric column errors, so an un-reconciled upgrade leaves the
/// row unsavable. `SQLite` needs no reconcile (its REAL affinity reads the JSON
/// text back fine) and a column already TEXT is a no-op. Shared by the
/// collection and global alter paths so both drift the same way.
pub(in crate::db::migrate) fn reconcile_scalar_list_column(
    conn: &dyn DbConnection,
    table: &str,
    col_name: &str,
    column_types: &HashMap<String, String>,
) -> Result<()> {
    let already_text = column_types
        .get(col_name)
        .is_none_or(|t| t.eq_ignore_ascii_case("TEXT"));

    if already_text || !conn.is_postgres() {
        return Ok(());
    }

    let sql = format!(
        "ALTER TABLE {} ALTER COLUMN {} TYPE TEXT USING {}::text",
        quote_ident(table),
        quote_ident(col_name),
        quote_ident(col_name)
    );
    info!("Reconciling scalar has-many column {table}.{col_name} to TEXT");

    conn.execute_ddl(&sql, &[])
        .with_context(|| format!("Failed to reconcile {col_name} to TEXT on {table}"))?;

    Ok(())
}

/// Add a column to `table` unless `existing` already contains `col_name`.
///
/// `col_def` is the full column definition including the (already-quoted) name —
/// e.g. `"\"scores\" TEXT NOT NULL"`. The one place the reconcile paths (collection
/// alter, global alter, locale/companion backfill, array sub-fields) emit the
/// `ALTER TABLE … ADD COLUMN` statement, so the quoting, logging, and
/// error-context can't drift between them.
pub(in crate::db::migrate) fn add_column_if_missing(
    conn: &dyn DbConnection,
    table: &str,
    col_name: &str,
    col_def: &str,
    existing: &HashSet<String>,
) -> Result<()> {
    if existing.contains(col_name) {
        return Ok(());
    }

    let sql = format!("ALTER TABLE {} ADD COLUMN {col_def}", quote_ident(table));

    info!("Adding column to {table}: {col_name}");

    conn.execute_ddl(&sql, &[])
        .with_context(|| format!("Failed to add column {col_name} to {table}"))?;

    Ok(())
}

/// The columns every crap-managed table carries whatever its definition says:
/// the row id, the timestamps, and everything the framework prefixes with `_`.
fn is_system_column(col: &str) -> bool {
    col.starts_with('_') || matches!(col, "id" | "created_at" | "updated_at")
}

/// Warn once per column a table holds that no field accounts for.
///
/// The one orphan-column report the collection, global and join-table sync
/// paths share — a removed field leaves its column behind on all three, and
/// removing it is `crap-cms db cleanup`'s decision rather than the boot's
/// (`SQLite` can't always drop a column, and the data is still in there).
pub(in crate::db::migrate) fn warn_orphan_columns(
    table: &str,
    existing: &HashSet<String>,
    expected: &HashSet<String>,
) {
    let mut orphans: Vec<&String> = existing
        .iter()
        .filter(|col| !expected.contains(*col) && !is_system_column(col))
        .collect();
    orphans.sort();

    for col in orphans {
        warn!("Column '{col}' exists in table '{table}' but not in Lua definition (not removed)");
    }
}

/// Refuse a column whose stored type differs from the definition's: on
/// `SQLite` the column would keep its old affinity and quietly store the new
/// type as text, on Postgres every later write would fail to bind. Shared by
/// the collection and global alter paths.
///
/// # Errors
///
/// Returns an error naming table, column, stored and expected type and the
/// manual migration path.
pub(in crate::db::migrate) fn check_type_mismatch(
    table: &str,
    column_types: &HashMap<String, String>,
    col_name: &str,
    expected_type: &str,
) -> Result<()> {
    let Some(db_type) = column_types.get(col_name) else {
        return Ok(());
    };

    if db_type.eq_ignore_ascii_case(expected_type) {
        return Ok(());
    }

    bail!(
        "Column '{col_name}' in table '{table}' is '{db_type}' but the field definition expects \
         '{expected_type}'. A field's type is not migrated automatically — rename the field \
         (which creates a new column) or migrate the column by hand; see the documentation on \
         changing a definition that has data."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every column the framework puts on a table itself is a system column,
    /// on a collection table (`_ref_count`, the timestamps) as on a join table
    /// (`_order`, `_locale`) — none of them is ever an orphan. A field column
    /// is not, whatever it is named.
    #[test]
    fn system_columns_are_recognized_on_every_kind_of_table() {
        for col in [
            "id",
            "created_at",
            "updated_at",
            "_order",
            "_locale",
            "_block_type",
            "_ref_count",
            "_deleted_at",
        ] {
            assert!(is_system_column(col), "{col} must count as a system column");
        }

        for col in ["title", "parent_id", "related_id", "data"] {
            assert!(!is_system_column(col), "{col} is a definition's column");
        }
    }
}
