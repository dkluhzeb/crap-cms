//! Back-reference shapes: result row + scan context.

use serde::Serialize;

use crate::config::LocaleConfig;
use crate::db::DbConnection;

/// A group of documents in one collection/global that reference a target via one field.
#[derive(Debug, Clone, Serialize)]
pub struct BackReference {
    pub owner_slug: String,
    pub owner_label: String,
    pub field_name: String,
    pub field_label: String,
    pub document_ids: Vec<String>,
    pub count: usize,
    pub is_global: bool,
}

impl BackReference {
    #[must_use]
    pub fn new(
        owner_slug: String,
        owner_label: String,
        field_name: String,
        field_label: String,
        document_ids: Vec<String>,
        is_global: bool,
    ) -> Self {
        let count = document_ids.len();
        Self {
            owner_slug,
            owner_label,
            field_name,
            field_label,
            document_ids,
            count,
            is_global,
        }
    }
}

/// Invariant context for a back-reference scan operation.
pub(super) struct BackRefScan<'a> {
    pub(super) conn: &'a dyn DbConnection,
    pub(super) target_collection: &'a str,
    pub(super) target_id: &'a str,
    pub(super) locale_config: &'a LocaleConfig,
    pub(super) owner_slug: &'a str,
    pub(super) owner_label: &'a str,
    pub(super) is_global: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_derives_count_from_document_ids() {
        let br = BackReference::new(
            "posts".into(),
            "Posts".into(),
            "tag".into(),
            "Tag".into(),
            vec!["a".into(), "b".into(), "c".into()],
            false,
        );
        assert_eq!(br.count, 3, "count must mirror document_ids length");
        assert_eq!(br.document_ids.len(), 3);
        assert!(!br.is_global);
    }

    #[test]
    fn new_with_no_ids_has_zero_count() {
        let br = BackReference::new(
            "settings".into(),
            "Settings".into(),
            "f".into(),
            "F".into(),
            vec![],
            true,
        );
        assert_eq!(br.count, 0);
        assert!(br.is_global);
    }
}
