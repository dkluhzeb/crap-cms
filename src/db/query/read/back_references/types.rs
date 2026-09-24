//! Back-reference shapes: result row + scan context.

use serde::Serialize;

use crate::{
    config::LocaleConfig,
    core::{Builder, FieldDefinition},
    db::DbConnection,
};

/// A group of documents in one collection/global that reference a target via one field.
#[derive(Debug, Clone, Serialize)]
pub struct BackReference {
    pub owner_slug: String,
    pub owner_label: String,
    /// The referring field's dotted path in the owner document — group and
    /// array names, and the block type of a block row (`meta.hero`,
    /// `slides.image`, `content.hero.bg_image`).
    pub field_name: String,
    pub field_label: String,
    pub document_ids: Vec<String>,
    pub count: usize,
    pub is_global: bool,
    /// The referring field as a query path — `field_name` without its block
    /// type segments (`content.bg_image`) — by which the viewer's read access
    /// to the field is judged. Never serialized.
    #[serde(skip)]
    pub query_path: String,
}

impl BackReference {
    /// Start a group of `owner_slug` documents referencing the target through
    /// `field_name`. The query path defaults to `field_name`.
    #[must_use]
    pub fn builder(
        owner_slug: impl Into<String>,
        field_name: impl Into<String>,
    ) -> BackReferenceBuilder {
        let field_name = field_name.into();

        BackReferenceBuilder {
            owner_slug: owner_slug.into(),
            owner_label: String::new(),
            query_path: field_name.clone(),
            field_name,
            field_label: String::new(),
            document_ids: Vec::new(),
            is_global: false,
        }
    }

    /// This group narrowed to `document_ids` (the ones a viewer may see).
    #[must_use]
    pub fn with_document_ids(self, document_ids: Vec<String>) -> Self {
        Self {
            count: document_ids.len(),
            document_ids,
            ..self
        }
    }
}

/// Builder for [`BackReference`]; `count` follows the document ids.
pub struct BackReferenceBuilder {
    owner_slug: String,
    owner_label: String,
    field_name: String,
    field_label: String,
    query_path: String,
    document_ids: Vec<String>,
    is_global: bool,
}

impl BackReferenceBuilder {
    #[must_use]
    pub fn owner_label(mut self, owner_label: impl Into<String>) -> Self {
        self.owner_label = owner_label.into();
        self
    }

    #[must_use]
    pub fn field_label(mut self, field_label: impl Into<String>) -> Self {
        self.field_label = field_label.into();
        self
    }

    #[must_use]
    pub fn query_path(mut self, query_path: impl Into<String>) -> Self {
        self.query_path = query_path.into();
        self
    }

    #[must_use]
    pub fn document_ids(mut self, document_ids: Vec<String>) -> Self {
        self.document_ids = document_ids;
        self
    }

    #[must_use]
    pub fn global(mut self, is_global: bool) -> Self {
        self.is_global = is_global;
        self
    }

    #[must_use]
    pub fn build(self) -> BackReference {
        BackReference {
            owner_slug: self.owner_slug,
            owner_label: self.owner_label,
            field_name: self.field_name,
            field_label: self.field_label,
            count: self.document_ids.len(),
            document_ids: self.document_ids,
            is_global: self.is_global,
            query_path: self.query_path,
        }
    }
}

/// Invariant context for a back-reference scan operation: the target being
/// referenced (required) and the owner collection or global being scanned.
#[derive(Builder)]
pub(super) struct BackRefScan<'a> {
    #[builder(required)]
    pub(super) conn: &'a dyn DbConnection,
    #[builder(required)]
    pub(super) locale_config: &'a LocaleConfig,
    #[builder(required)]
    pub(super) target_collection: &'a str,
    #[builder(required)]
    pub(super) target_id: &'a str,
    /// The owner's fields, against which a column's localization is decided.
    #[builder(default = &[])]
    pub(super) root_fields: &'a [FieldDefinition],
    #[builder(default = "")]
    pub(super) owner_slug: &'a str,
    #[builder(default = "")]
    pub(super) owner_label: &'a str,
    pub(super) is_global: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_derives_count_from_document_ids() {
        let br = BackReference::builder("posts", "tag")
            .owner_label("Posts")
            .field_label("Tag")
            .document_ids(vec!["a".into(), "b".into(), "c".into()])
            .build();
        assert_eq!(br.count, 3, "count must mirror document_ids length");
        assert_eq!(br.document_ids.len(), 3);
        assert!(!br.is_global);
    }

    #[test]
    fn build_with_no_ids_has_zero_count() {
        let br = BackReference::builder("settings", "f").global(true).build();
        assert_eq!(br.count, 0);
        assert!(br.is_global);
    }

    #[test]
    fn query_path_defaults_to_the_field_name() {
        let br = BackReference::builder("posts", "meta.hero").build();
        assert_eq!(br.query_path, "meta.hero");

        let br = BackReference::builder("posts", "content.hero.bg")
            .query_path("content.bg")
            .build();
        assert_eq!(br.query_path, "content.bg");
    }

    #[test]
    fn with_document_ids_recounts() {
        let br = BackReference::builder("posts", "tag")
            .document_ids(vec!["a".into(), "b".into()])
            .build()
            .with_document_ids(vec!["a".into()]);
        assert_eq!(br.count, 1);
        assert_eq!(br.document_ids, vec!["a".to_string()]);
    }

    /// The query path is internal: the report the admin fetches never
    /// carries it.
    #[test]
    fn query_path_is_not_serialized() {
        let br = BackReference::builder("posts", "tag").build();
        let json = serde_json::to_value(&br).unwrap();
        assert!(json.get("query_path").is_none(), "{json}");
    }
}
