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

/// Simple query for array/blocks `parent_id` lookups.
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

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::core::LocalizedString;
    use crate::core::field::{FieldAdmin, FieldType};
    use crate::db::InMemoryConn;

    fn labelled(name: &str, label: Option<&str>) -> FieldDefinition {
        let mut b = FieldDefinition::builder(name, FieldType::Text);
        if let Some(l) = label {
            b = b.admin(
                FieldAdmin::builder()
                    .label(LocalizedString::Plain(l.to_string()))
                    .build(),
            );
        }
        b.build()
    }

    #[test]
    fn label_prefers_non_empty_admin_label() {
        assert_eq!(
            field_display_label(&labelled("first_name", Some("Given Name"))),
            "Given Name"
        );
    }

    #[test]
    fn label_falls_back_to_title_cased_name_when_absent() {
        assert_eq!(
            field_display_label(&labelled("first_name", None)),
            "First Name"
        );
    }

    #[test]
    fn empty_admin_label_falls_back_to_name() {
        assert_eq!(
            field_display_label(&labelled("first_name", Some(""))),
            "First Name"
        );
    }

    fn refs_conn() -> InMemoryConn {
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE refs (id TEXT); INSERT INTO refs VALUES ('a'), ('b'), ('t1');");
        conn
    }

    #[test]
    fn query_ids_drops_self_reference_same_collection_and_id() {
        let conn = refs_conn();
        // owner == target collection AND a row id == target id → that row is
        // the document itself, so it's filtered out.
        let ids = query_ids(
            &conn,
            "SELECT id FROM refs ORDER BY id",
            &[],
            "posts",
            "t1",
            "posts",
            false,
        );
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn query_ids_keeps_same_id_from_a_different_collection() {
        let conn = refs_conn();
        let ids = query_ids(
            &conn,
            "SELECT id FROM refs ORDER BY id",
            &[],
            "posts",
            "t1",
            "tags",
            false,
        );
        assert_eq!(ids, vec!["a", "b", "t1"]);
    }

    #[test]
    fn query_ids_global_keeps_everything() {
        let conn = refs_conn();
        let ids = query_ids(
            &conn,
            "SELECT id FROM refs ORDER BY id",
            &[],
            "settings",
            "t1",
            "settings",
            true,
        );
        assert_eq!(ids, vec!["a", "b", "t1"]);
    }

    #[test]
    fn query_ids_simple_collects_the_id_column() {
        let conn = refs_conn();
        let ids = query_ids_simple(&conn, "SELECT id FROM refs WHERE id != ?1 ORDER BY id", "a");
        assert_eq!(ids, vec!["b", "t1"]);
    }
}
