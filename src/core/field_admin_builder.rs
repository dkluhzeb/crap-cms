//! Builder for [`FieldAdmin`](crate::core::field::FieldAdmin).

use crate::core::field::{FieldAdmin, LocalizedString};

/// Builder for [`FieldAdmin`].
///
/// All fields are optional and default via [`FieldAdmin::default()`].
pub struct FieldAdminBuilder {
    inner: FieldAdmin,
}

impl FieldAdminBuilder {
    pub fn new() -> Self {
        Self {
            inner: FieldAdmin::default(),
        }
    }

    pub fn label(mut self, v: LocalizedString) -> Self {
        self.inner.label = Some(v);
        self
    }

    pub fn placeholder(mut self, v: LocalizedString) -> Self {
        self.inner.placeholder = Some(v);
        self
    }

    pub fn description(mut self, v: LocalizedString) -> Self {
        self.inner.description = Some(v);
        self
    }

    pub fn hidden(mut self, v: bool) -> Self {
        self.inner.hidden = v;
        self
    }

    pub fn readonly(mut self, v: bool) -> Self {
        self.inner.readonly = v;
        self
    }

    pub fn width(mut self, v: impl Into<String>) -> Self {
        self.inner.width = Some(v.into());
        self
    }

    pub fn collapsed(mut self, v: bool) -> Self {
        self.inner.collapsed = v;
        self
    }

    pub fn label_field(mut self, v: impl Into<String>) -> Self {
        self.inner.label_field = Some(v.into());
        self
    }

    pub fn row_label(mut self, v: impl Into<String>) -> Self {
        self.inner.row_label = Some(v.into());
        self
    }

    pub fn labels_singular(mut self, v: LocalizedString) -> Self {
        self.inner.labels_singular = Some(v);
        self
    }

    pub fn labels_plural(mut self, v: LocalizedString) -> Self {
        self.inner.labels_plural = Some(v);
        self
    }

    pub fn position(mut self, v: impl Into<String>) -> Self {
        self.inner.position = Some(v.into());
        self
    }

    pub fn condition(mut self, v: impl Into<String>) -> Self {
        self.inner.condition = Some(v.into());
        self
    }

    pub fn step(mut self, v: impl Into<String>) -> Self {
        self.inner.step = Some(v.into());
        self
    }

    pub fn rows(mut self, v: u32) -> Self {
        self.inner.rows = Some(v);
        self
    }

    pub fn language(mut self, v: impl Into<String>) -> Self {
        self.inner.language = Some(v.into());
        self
    }

    pub fn features(mut self, v: Vec<String>) -> Self {
        self.inner.features = v;
        self
    }

    pub fn picker(mut self, v: impl Into<String>) -> Self {
        self.inner.picker = Some(v.into());
        self
    }

    pub fn richtext_format(mut self, v: impl Into<String>) -> Self {
        self.inner.richtext_format = Some(v.into());
        self
    }

    pub fn nodes(mut self, v: Vec<String>) -> Self {
        self.inner.nodes = v;
        self
    }

    pub fn build(self) -> FieldAdmin {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_field_admin_with_defaults() {
        let admin = FieldAdminBuilder::new().build();
        assert!(admin.label.is_none());
        assert!(!admin.hidden);
        assert!(!admin.readonly);
        assert!(admin.collapsed);
        assert!(admin.features.is_empty());
    }

    #[test]
    fn builds_field_admin_with_overrides() {
        let admin = FieldAdminBuilder::new()
            .label(LocalizedString::Plain("Title".into()))
            .hidden(true)
            .readonly(true)
            .width("50%")
            .collapsed(false)
            .position("sidebar")
            .rows(12)
            .build();
        assert!(admin.label.is_some());
        assert!(admin.hidden);
        assert!(admin.readonly);
        assert_eq!(admin.width.as_deref(), Some("50%"));
        assert!(!admin.collapsed);
        assert_eq!(admin.position.as_deref(), Some("sidebar"));
        assert_eq!(admin.rows, Some(12));
    }

    #[test]
    fn builds_field_admin_with_richtext_options() {
        let admin = FieldAdminBuilder::new()
            .richtext_format("json")
            .features(vec!["bold".into(), "italic".into()])
            .nodes(vec!["cta".into()])
            .build();
        assert_eq!(admin.richtext_format.as_deref(), Some("json"));
        assert_eq!(admin.features.len(), 2);
        assert_eq!(admin.nodes, vec!["cta"]);
    }
}
