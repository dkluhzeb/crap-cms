//! Shared helpers for join table operations.

use anyhow::{Context as _, Result};

use crate::db::{DbConnection, DbValue};

/// Delete rows from a junction/join table for a given parent, optionally filtered by locale.
pub(super) fn delete_junction_rows(
    conn: &dyn DbConnection,
    table_name: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<()> {
    if let Some(loc) = locale {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));

        conn.execute(
            &format!(
                "DELETE FROM \"{}\" WHERE parent_id = {p1} AND _locale = {p2}",
                table_name
            ),
            &[
                DbValue::Text(parent_id.to_string()),
                DbValue::Text(loc.to_string()),
            ],
        )
        .with_context(|| format!("Failed to clear join table {}", table_name))?;
    } else {
        let p1 = conn.placeholder(1);

        conn.execute(
            &format!("DELETE FROM \"{}\" WHERE parent_id = {p1}", table_name),
            &[DbValue::Text(parent_id.to_string())],
        )
        .with_context(|| format!("Failed to clear join table {}", table_name))?;
    }

    Ok(())
}
