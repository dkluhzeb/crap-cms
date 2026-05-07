//! Admin UI display options for collections and globals.

use serde::{Deserialize, Serialize};

/// Admin UI display options (title field, default sort, visibility, searchable fields).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdminConfig {
    /// The field name to use as the title for documents in the admin UI.
    #[serde(default)]
    pub use_as_title: Option<String>,
    /// The default sort order for document lists (e.g., "-createdAt").
    #[serde(default)]
    pub default_sort: Option<String>,
    /// Whether to hide this collection from the admin sidebar.
    #[serde(default)]
    pub hidden: bool,
    /// List of fields that should be searchable in the admin list view.
    #[serde(default)]
    pub list_searchable_fields: Vec<String>,
}

impl AdminConfig {
    /// Create a new default admin configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder for admin configuration.
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
}

impl AdminConfigBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub fn use_as_title(mut self, v: Option<String>) -> Self {
        self.use_as_title = v;

        self
    }

    pub fn default_sort(mut self, v: Option<String>) -> Self {
        self.default_sort = v;

        self
    }

    pub fn hidden(mut self, v: bool) -> Self {
        self.hidden = v;

        self
    }

    pub fn list_searchable_fields(mut self, v: Vec<String>) -> Self {
        self.list_searchable_fields = v;

        self
    }

    pub fn build(self) -> AdminConfig {
        AdminConfig {
            use_as_title: self.use_as_title,
            default_sort: self.default_sort,
            hidden: self.hidden,
            list_searchable_fields: self.list_searchable_fields,
        }
    }
}
