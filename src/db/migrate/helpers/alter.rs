//! The single `ALTER TABLE … ADD COLUMN` chokepoint for reconcile paths.

use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result};
use tracing::info;

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
