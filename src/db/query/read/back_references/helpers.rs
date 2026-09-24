//! ID-collecting query helpers and field-label rendering shared by the
//! scan submodules.

use tracing::debug;

use super::types::BackRefScan;
use crate::{core::FieldDefinition, db::DbValue};

/// Get the display label for a field (admin label or title-cased name).
pub(in crate::db::query::read) fn field_display_label(field: &FieldDefinition) -> String {
    field.resolved_label()
}

/// Per-query cap on collected back-reference ids. This list is a
/// display/diagnostic aid (the delete page shows the authoritative O(1)
/// `_ref_count`, and deletion is blocked by that count, not by this list), so a
/// document referenced by an enormous number of rows must not make one admin
/// request materialize an unbounded row set. Bounding at the shared query
/// chokepoint keeps the DB return and the accumulated `Vec` bounded across every
/// scanner (has-one / has-many / array / blocks / poly).
const MAX_BACK_REF_IDS_PER_QUERY: usize = 1000;

/// Execute a query on `scan`'s connection and collect `id` column values,
/// filtering out a reference of the scan's target to itself.
pub(super) fn query_ids(scan: &BackRefScan, sql: &str, params: &[DbValue]) -> Vec<String> {
    // Append the cap at the single query chokepoint: these are simple
    // `SELECT id FROM … WHERE …` statements with no trailing clause, so a
    // `LIMIT` suffix is safe on both backends.
    let sql = &format!("{sql} LIMIT {MAX_BACK_REF_IDS_PER_QUERY}");

    match scan.conn.query_all(sql, params) {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| row.opt_text_at(0))
            // Skip self-references (same collection, same ID)
            .filter(|id| {
                scan.is_global || id != scan.target_id || scan.owner_slug != scan.target_collection
            })
            .collect(),
        Err(e) => {
            debug!("Back-ref scan query failed: {}", e);
            Vec::new()
        }
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::{
        config::LocaleConfig,
        core::{
            LocalizedString,
            field::{FieldAdmin, FieldType},
        },
        db::InMemoryConn,
    };

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

    /// The ids `SELECT id FROM refs` yields when scanning `owner` (a global
    /// when `is_global`) for references to `posts/t1`.
    fn scanned_ids(owner: &str, is_global: bool) -> Vec<String> {
        let conn = refs_conn();
        let locale_config = LocaleConfig::default();
        let scan = BackRefScan::builder(&conn, &locale_config, "posts", "t1")
            .owner_slug(owner)
            .is_global(is_global)
            .build();

        query_ids(&scan, "SELECT id FROM refs ORDER BY id", &[])
    }

    #[test]
    fn query_ids_drops_self_reference_same_collection_and_id() {
        // owner == target collection AND a row id == target id → that row is
        // the document itself, so it's filtered out.
        assert_eq!(scanned_ids("posts", false), vec!["a", "b"]);
    }

    #[test]
    fn query_ids_keeps_same_id_from_a_different_collection() {
        assert_eq!(scanned_ids("tags", false), vec!["a", "b", "t1"]);
    }

    #[test]
    fn query_ids_global_keeps_everything() {
        assert_eq!(scanned_ids("posts", true), vec!["a", "b", "t1"]);
    }
}
