//! Admin UI display options for collections and globals.

use serde::{Deserialize, Serialize};

use crate::typegen::lua::LuaAnnotation;

/// Admin UI display options (title field, default sort, visibility, searchable fields).
#[derive(Debug, Clone, Serialize, Deserialize, Default, LuaAnnotation)]
#[lua(class = "crap.AdminConfig")]
pub struct AdminConfig {
    /// Field name to use as row label in lists.
    #[serde(default)]
    pub use_as_title: Option<String>,
    /// Default sort field (prefix with "-" for desc).
    #[serde(default)]
    pub default_sort: Option<String>,
    /// Hide from admin sidebar (default: false).
    #[serde(default)]
    #[lua(optional)]
    pub hidden: bool,
    /// Fields searchable in the list view.
    #[serde(default)]
    #[lua(optional)]
    pub list_searchable_fields: Vec<String>,
    /// Default columns shown in the list view, in order. Empty = the built-in
    /// default (`_status` if the collection has drafts, plus `created_at`). A
    /// per-user column selection overrides this. Entries may be field names or
    /// the meta columns `created_at` / `updated_at` / `_status`.
    #[serde(default)]
    #[lua(optional)]
    pub list_columns: Vec<String>,
}

impl AdminConfig {
    /// Create a new default admin configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder for admin configuration.
    #[must_use]
    pub fn builder() -> AdminConfigBuilder {
        AdminConfigBuilder::new()
    }
}

/// Builder for [`AdminConfig`]. Created via [`AdminConfig::builder`].
#[derive(Default)]
pub struct AdminConfigBuilder {
    use_as_title: Option<String>,
    default_sort: Option<String>,
    hidden: bool,
    list_searchable_fields: Vec<String>,
    list_columns: Vec<String>,
}

impl AdminConfigBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn use_as_title(mut self, v: Option<String>) -> Self {
        self.use_as_title = v;

        self
    }

    #[must_use]
    pub fn default_sort(mut self, v: Option<String>) -> Self {
        self.default_sort = v;

        self
    }

    #[must_use]
    pub fn hidden(mut self, v: bool) -> Self {
        self.hidden = v;

        self
    }

    #[must_use]
    pub fn list_searchable_fields(mut self, v: Vec<String>) -> Self {
        self.list_searchable_fields = v;

        self
    }

    #[must_use]
    pub fn list_columns(mut self, v: Vec<String>) -> Self {
        self.list_columns = v;

        self
    }

    #[must_use]
    pub fn build(self) -> AdminConfig {
        AdminConfig {
            use_as_title: self.use_as_title,
            default_sort: self.default_sort,
            hidden: self.hidden,
            list_searchable_fields: self.list_searchable_fields,
            list_columns: self.list_columns,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty_and_visible() {
        let c = AdminConfig::new();
        assert!(c.use_as_title.is_none());
        assert!(c.default_sort.is_none());
        assert!(!c.hidden);
        assert!(c.list_searchable_fields.is_empty());
        assert!(c.list_columns.is_empty());
    }

    /// Distinct value per field guards against a cross-wired `build()`.
    #[test]
    fn builder_wires_each_field_to_its_own_slot() {
        let c = AdminConfig::builder()
            .use_as_title(Some("title".into()))
            .default_sort(Some("-created_at".into()))
            .hidden(true)
            .list_searchable_fields(vec!["title".into(), "body".into()])
            .list_columns(vec!["title".into(), "_status".into()])
            .build();
        assert_eq!(c.use_as_title.as_deref(), Some("title"));
        assert_eq!(c.default_sort.as_deref(), Some("-created_at"));
        assert!(c.hidden);
        assert_eq!(c.list_searchable_fields, vec!["title", "body"]);
        assert_eq!(c.list_columns, vec!["title", "_status"]);
    }
}
