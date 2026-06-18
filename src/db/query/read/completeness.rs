//! Reads backing localized-completeness validation: the existing row's column
//! values and join-table presence for a parent + locale.
//!
//! The validation layer (`hooks::lifecycle::validation::completeness`) decides
//! WHICH columns / join tables a required localized field needs; these execute
//! the lookups, keeping the SQL in the `db` module.

use std::collections::HashMap;

use anyhow::Result;

use crate::db::{DbConnection, DbValue};

/// Whether the localized join `table` has at least one row for `parent_id` in
/// `locale` — i.e. whether a required localized array/blocks/has-many field is
/// already satisfied on the existing row for that locale.
///
/// # Errors
///
/// Returns a backend error if the query fails.
pub fn localized_join_row_exists(
    conn: &dyn DbConnection,
    table: &str,
    parent_id: &str,
    locale: &str,
) -> Result<bool> {
    let sql = format!(
        "SELECT 1 FROM \"{table}\" WHERE parent_id = {} AND _locale = {} LIMIT 1",
        conn.placeholder(1),
        conn.placeholder(2)
    );

    Ok(conn
        .query_one(
            &sql,
            &[
                DbValue::Text(parent_id.to_string()),
                DbValue::Text(locale.to_string()),
            ],
        )?
        .is_some())
}

/// Fetch `cols` of row `id` from `table` as a name→value map. Returns `None`
/// when `cols` is empty or the row doesn't exist.
///
/// `cols` are locale-suffixed column names produced from validated field names
/// and sanitized locales, and `table` is a validated collection slug — both
/// safe to interpolate.
///
/// # Errors
///
/// Returns a backend error if the query fails.
pub fn fetch_row_columns(
    conn: &dyn DbConnection,
    table: &str,
    cols: &[String],
    id: &str,
) -> Result<Option<HashMap<String, DbValue>>> {
    if cols.is_empty() {
        return Ok(None);
    }

    let col_list = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {col_list} FROM \"{table}\" WHERE id = {}",
        conn.placeholder(1)
    );

    let Some(row) = conn.query_one(&sql, &[DbValue::Text(id.to_string())])? else {
        return Ok(None);
    };

    let mut map = HashMap::new();
    for c in cols {
        if let Some(v) = row.get_named(c) {
            map.insert(c.clone(), v.clone());
        }
    }

    Ok(Some(map))
}
