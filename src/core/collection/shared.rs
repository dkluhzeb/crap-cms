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
    /// Create a new default MCP configuration.
    pub fn new() -> Self {
        Self::default()
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
    /// Create a new default labels configuration.
    pub fn new() -> Self {
        Self::default()
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
