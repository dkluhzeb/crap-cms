//! The single `ALTER TABLE … ADD COLUMN` chokepoint for reconcile paths.

use std::collections::HashSet;

use anyhow::{Context as _, Result};
use tracing::info;

use crate::db::DbConnection;
use crate::db::query::helpers::quote_ident;

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
