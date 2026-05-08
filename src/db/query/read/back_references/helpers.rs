//! ID-collecting query helpers and field-label rendering shared by the
//! scan submodules.

use crate::core::{FieldDefinition, field::to_title_case};
use crate::db::{DbConnection, DbValue};

/// Get the display label for a field (admin label or title-cased name).
pub(in crate::db::query::read) fn field_display_label(field: &FieldDefinition) -> String {
    field
        .admin
        .label
        .as_ref()
        .map(|l| l.resolve_default().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| to_title_case(&field.name))
}

/// Execute a query and collect `id` column values, filtering out self-references.
pub(super) fn query_ids(
    conn: &dyn DbConnection,
    sql: &str,
    params: &[DbValue],
    owner_slug: &str,
    target_id: &str,
    target_collection: &str,
    is_global: bool,
) -> Vec<String> {
    match conn.query_all(sql, params) {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| row.opt_text_at(0))
            // Skip self-references (same collection, same ID)
            .filter(|id| is_global || id != target_id || owner_slug != target_collection)
            .collect(),
        Err(e) => {
            tracing::debug!("Back-ref scan query failed: {}", e);
            Vec::new()
        }
    }
}

/// Simple query for array/blocks parent_id lookups.
pub(super) fn query_ids_simple(conn: &dyn DbConnection, sql: &str, value: &str) -> Vec<String> {
    let params = vec![DbValue::Text(value.to_string())];
    match conn.query_all(sql, &params) {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| row.opt_text_at(0))
            .collect(),
        Err(e) => {
            tracing::debug!("Back-ref scan query failed: {}", e);
            Vec::new()
        }
    }
}

/// Query with arbitrary params, returning collected IDs.
pub(super) fn query_ids_simple_params(
    conn: &dyn DbConnection,
    sql: &str,
    params: &[DbValue],
) -> Vec<String> {
    match conn.query_all(sql, params) {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| row.opt_text_at(0))
            .collect(),
        Err(e) => {
            tracing::debug!("Back-ref scan query failed: {}", e);
            Vec::new()
        }
    }
}
