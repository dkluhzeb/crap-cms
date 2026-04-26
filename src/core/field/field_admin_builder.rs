//! Builder for [`FieldAdmin`](super::FieldAdmin).

use crate::core::{FieldAdmin, LocalizedString};

/// Builder for [`FieldAdmin`].
///
/// All fields are optional and default via [`FieldAdmin::default()`].
pub struct FieldAdminBuilder {
    inner: FieldAdmin,
}

impl Default for FieldAdminBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl FieldAdminBuilder {
    /// Create a new `FieldAdminBuilder`.
    pub fn new() -> Self {
        Self {
            inner: FieldAdmin::default(),
        }
    }

    /// Set the localized label for the field.
    pub fn label(mut self, v: LocalizedString) -> Self {
        self.inner.label = Some(v);
        self
    }

    /// Set the localized placeholder text for the field.
    pub fn placeholder(mut self, v: LocalizedString) -> Self {
        self.inner.placeholder = Some(v);
        self
    }

    /// Set the localized description text for the field.
    pub fn description(mut self, v: LocalizedString) -> Self {
        self.inner.description = Some(v);
        self
    }

    /// Set whether the field is hidden in the admin UI.
    pub fn hidden(mut self, v: bool) -> Self {
        self.inner.hidden = v;
        self
    }

    /// Set whether the field is read-only in the admin UI.
    pub fn readonly(mut self, v: bool) -> Self {
        self.inner.readonly = v;
        self
    }

    /// Set the CSS width for the field container (e.g., "50%").
    pub fn width(mut self, v: impl Into<String>) -> Self {
        self.inner.width = Some(v.into());
        self
    }

    /// Set whether the field starts collapsed.
    pub fn collapsed(mut self, v: bool) -> Self {
        self.inner.collapsed = v;
        self
    }

    /// Set the sub-field name to use as row label (for arrays/blocks).
    pub fn label_field(mut self, v: impl Into<String>) -> Self {
        self.inner.label_field = Some(v.into());
        self
    }

    /// Set the Lua function ref for computed row labels.
    pub fn row_label(mut self, v: impl Into<String>) -> Self {
        self.inner.row_label = Some(v.into());
        self
    }

    /// Set custom singular label for row items.
    pub fn labels_singular(mut self, v: LocalizedString) -> Self {
        self.inner.labels_singular = Some(v);
        self
    }

    /// Set custom plural label for the field.
    pub fn labels_plural(mut self, v: LocalizedString) -> Self {
        self.inner.labels_plural = Some(v);
        self
    }

    /// Set field position in layout ("main" or "sidebar").
    pub fn position(mut self, v: impl Into<String>) -> Self {
        self.inner.position = Some(v.into());
        self
    }

    /// Set Lua function ref for conditional visibility.
    pub fn condition(mut self, v: impl Into<String>) -> Self {
        self.inner.condition = Some(v.into());
        self
    }

    /// Set step attribute for number inputs.
    pub fn step(mut self, v: impl Into<String>) -> Self {
        self.inner.step = Some(v.into());
        self
    }

    /// Set number of visible rows for textarea fields.
    pub fn rows(mut self, v: u32) -> Self {
        self.inner.rows = Some(v);
        self
    }

    /// Set language mode for code fields.
    pub fn language(mut self, v: impl Into<String>) -> Self {
        self.inner.language = Some(v.into());
        self
    }

    /// Set the allow-list of languages the editor can pick from at edit time
    /// for code fields. When non-empty, the form renders a language picker.
    pub fn languages(mut self, v: Vec<String>) -> Self {
        self.inner.languages = v;
        self
    }

    /// Set enabled toolbar features for richtext fields.
    pub fn features(mut self, v: Vec<String>) -> Self {
        self.inner.features = v;
        self
    }

    /// Set picker style for blocks fields ("select" or "card").
    pub fn picker(mut self, v: impl Into<String>) -> Self {
        self.inner.picker = Some(v.into());
        self
    }

    /// Set storage format for richtext fields ("html" or "json").
    pub fn richtext_format(mut self, v: impl Into<String>) -> Self {
        self.inner.richtext_format = Some(v.into());
        self
    }

    /// Set available custom node types for richtext fields.
    pub fn nodes(mut self, v: Vec<String>) -> Self {
        self.inner.nodes = v;
        self
    }

    /// Set whether the field allows vertical resizing (textarea/richtext).
    pub fn resizable(mut self, v: bool) -> Self {
        self.inner.resizable = v;
        self
    }

    /// Build the final `FieldAdmin`.
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
        assert!(admin.resizable);
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
            .resizable(false)
            .build();
        assert!(admin.label.is_some());
        assert!(admin.hidden);
        assert!(admin.readonly);
        assert_eq!(admin.width.as_deref(), Some("50%"));
        assert!(!admin.collapsed);
        assert_eq!(admin.position.as_deref(), Some("sidebar"));
        assert_eq!(admin.rows, Some(12));
        assert!(!admin.resizable);
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
