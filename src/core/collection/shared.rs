//! Types shared between collections and globals.

use super::super::field::LocalizedString;
use serde::{Deserialize, Serialize};

/// MCP-specific configuration for a collection or global.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct McpConfig {
    /// Description used in MCP tool descriptions for this collection/global.
    pub description: Option<String>,
}

impl McpConfig {
    /// Create a new MCP configuration with the given description.
    pub fn new(description: Option<String>) -> Self {
        Self { description }
    }
}

/// Configuration for document versioning and drafts on a collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionsConfig {
    /// Enable draft/publish workflow with `_status` field.
    #[serde(default)]
    pub drafts: bool,
    /// Maximum versions to keep per document (0 = unlimited).
    #[serde(default)]
    pub max_versions: u32,
}

impl VersionsConfig {
    /// Create a new versioning configuration.
    pub fn new(drafts: bool, max_versions: u32) -> Self {
        Self {
            drafts,
            max_versions,
        }
    }
}

/// Controls live event broadcasting for a collection or global.
/// `None` = enabled (broadcast all events).
/// `Some(LiveSetting::Disabled)` = never broadcast.
/// `Some(LiveSetting::Function(ref))` = Lua function decides per-event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LiveSetting {
    /// Disable all live event broadcasting for this collection/global.
    Disabled,
    /// Use a Lua function to determine if an event should be broadcast.
    Function(String),
}

/// Lua function references for access control (read/create/update/delete).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Access {
    /// Lua function for read access control.
    #[serde(default)]
    pub read: Option<String>,
    /// Lua function for create access control.
    #[serde(default)]
    pub create: Option<String>,
    /// Lua function for update access control.
    #[serde(default)]
    pub update: Option<String>,
    /// Lua function for delete access control.
    #[serde(default)]
    pub delete: Option<String>,
}

impl Access {
    /// Create a new default access control configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder for access control configuration.
    pub fn builder() -> AccessBuilder {
        AccessBuilder::new()
    }
}

/// Builder for [`Access`]. Created via [`Access::builder`].
pub struct AccessBuilder {
    read: Option<String>,
    create: Option<String>,
    update: Option<String>,
    delete: Option<String>,
}

impl AccessBuilder {
    fn new() -> Self {
        Self {
            read: None,
            create: None,
            update: None,
            delete: None,
        }
    }

    pub fn read(mut self, read: Option<String>) -> Self {
        self.read = read;
        self
    }

    pub fn create(mut self, create: Option<String>) -> Self {
        self.create = create;
        self
    }

    pub fn update(mut self, update: Option<String>) -> Self {
        self.update = update;
        self
    }

    pub fn delete(mut self, delete: Option<String>) -> Self {
        self.delete = delete;
        self
    }

    pub fn build(self) -> Access {
        Access {
            read: self.read,
            create: self.create,
            update: self.update,
            delete: self.delete,
        }
    }
}

/// Lua function references for lifecycle hooks.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Hooks {
    /// Functions called before document validation.
    #[serde(default)]
    pub before_validate: Vec<String>,
    /// Functions called before a document is changed (created or updated).
    #[serde(default)]
    pub before_change: Vec<String>,
    /// Functions called after a document is changed.
    #[serde(default)]
    pub after_change: Vec<String>,
    /// Functions called before a document is read.
    #[serde(default)]
    pub before_read: Vec<String>,
    /// Functions called after a document is read.
    #[serde(default)]
    pub after_read: Vec<String>,
    /// Functions called before a document is deleted.
    #[serde(default)]
    pub before_delete: Vec<String>,
    /// Functions called after a document is deleted.
    #[serde(default)]
    pub after_delete: Vec<String>,
    /// Functions called before an event is broadcast.
    #[serde(default)]
    pub before_broadcast: Vec<String>,
}

impl Hooks {
    /// Create a new default hooks configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder for hooks configuration.
    pub fn builder() -> HooksBuilder {
        HooksBuilder::new()
    }
}

/// Builder for [`Hooks`]. Created via [`Hooks::builder`].
#[derive(Default)]
pub struct HooksBuilder {
    before_validate: Vec<String>,
    before_change: Vec<String>,
    after_change: Vec<String>,
    before_read: Vec<String>,
    after_read: Vec<String>,
    before_delete: Vec<String>,
    after_delete: Vec<String>,
    before_broadcast: Vec<String>,
}

impl HooksBuilder {
    fn new() -> Self {
        Self::default()
    }

    pub fn before_validate(mut self, v: Vec<String>) -> Self {
        self.before_validate = v;
        self
    }

    pub fn before_change(mut self, v: Vec<String>) -> Self {
        self.before_change = v;
        self
    }

    pub fn after_change(mut self, v: Vec<String>) -> Self {
        self.after_change = v;
        self
    }

    pub fn before_read(mut self, v: Vec<String>) -> Self {
        self.before_read = v;
        self
    }

    pub fn after_read(mut self, v: Vec<String>) -> Self {
        self.after_read = v;
        self
    }

    pub fn before_delete(mut self, v: Vec<String>) -> Self {
        self.before_delete = v;
        self
    }

    pub fn after_delete(mut self, v: Vec<String>) -> Self {
        self.after_delete = v;
        self
    }

    pub fn before_broadcast(mut self, v: Vec<String>) -> Self {
        self.before_broadcast = v;
        self
    }

    pub fn build(self) -> Hooks {
        Hooks {
            before_validate: self.before_validate,
            before_change: self.before_change,
            after_change: self.after_change,
            before_read: self.before_read,
            after_read: self.after_read,
            before_delete: self.before_delete,
            after_delete: self.after_delete,
            before_broadcast: self.before_broadcast,
        }
    }
}

/// Human-readable singular/plural labels for the admin UI.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Labels {
    /// Singular label for the collection (e.g., "Post").
    #[serde(default)]
    pub singular: Option<LocalizedString>,
    /// Plural label for the collection (e.g., "Posts").
    #[serde(default)]
    pub plural: Option<LocalizedString>,
}

impl Labels {
    /// Create a new labels configuration with singular and plural forms.
    pub fn new(singular: Option<LocalizedString>, plural: Option<LocalizedString>) -> Self {
        Self { singular, plural }
    }
}

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
pub struct AdminConfigBuilder {
    use_as_title: Option<String>,
    default_sort: Option<String>,
    hidden: bool,
    list_searchable_fields: Vec<String>,
}

impl AdminConfigBuilder {
    fn new() -> Self {
        Self {
            use_as_title: None,
            default_sort: None,
            hidden: false,
            list_searchable_fields: Vec::new(),
        }
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

/// A compound index definition (multi-column, optionally unique).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDefinition {
    /// List of field names that make up the index.
    pub fields: Vec<String>,
    /// Whether this index should enforce uniqueness.
    #[serde(default)]
    pub unique: bool,
}

impl IndexDefinition {
    /// Create a new index definition for the given fields.
    pub fn new(fields: Vec<String>) -> Self {
        Self {
            fields,
            unique: false,
        }
    }
}
