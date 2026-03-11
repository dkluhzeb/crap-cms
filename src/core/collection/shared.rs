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
    Disabled,
    Function(String),
}

/// Lua function references for access control (read/create/update/delete).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Access {
    #[serde(default)]
    pub read: Option<String>,
    #[serde(default)]
    pub create: Option<String>,
    #[serde(default)]
    pub update: Option<String>,
    #[serde(default)]
    pub delete: Option<String>,
}

impl Access {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Lua function references for lifecycle hooks.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Hooks {
    #[serde(default)]
    pub before_validate: Vec<String>,
    #[serde(default)]
    pub before_change: Vec<String>,
    #[serde(default)]
    pub after_change: Vec<String>,
    #[serde(default)]
    pub before_read: Vec<String>,
    #[serde(default)]
    pub after_read: Vec<String>,
    #[serde(default)]
    pub before_delete: Vec<String>,
    #[serde(default)]
    pub after_delete: Vec<String>,
    #[serde(default)]
    pub before_broadcast: Vec<String>,
}

impl Hooks {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Human-readable singular/plural labels for the admin UI.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Labels {
    #[serde(default)]
    pub singular: Option<LocalizedString>,
    #[serde(default)]
    pub plural: Option<LocalizedString>,
}

impl Labels {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Admin UI display options (title field, default sort, visibility, searchable fields).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdminConfig {
    #[serde(default)]
    pub use_as_title: Option<String>,
    #[serde(default)]
    pub default_sort: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub list_searchable_fields: Vec<String>,
}

impl AdminConfig {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A compound index definition (multi-column, optionally unique).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDefinition {
    pub fields: Vec<String>,
    #[serde(default)]
    pub unique: bool,
}

impl IndexDefinition {
    pub fn new(fields: Vec<String>) -> Self {
        Self {
            fields,
            unique: false,
        }
    }
}
