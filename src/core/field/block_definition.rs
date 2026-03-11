//! Block and tab definitions for Blocks/Tabs layout fields.

use super::{FieldDefinition, LocalizedString};
use serde::{Deserialize, Serialize};

/// A single tab within a Tabs layout field.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FieldTab {
    /// The display label for this tab.
    #[serde(default)]
    pub label: String,
    /// Optional help text or description for the tab.
    #[serde(default)]
    pub description: Option<String>,
    /// The list of fields to display within this tab.
    #[serde(default)]
    pub fields: Vec<FieldDefinition>,
}

impl FieldTab {
    /// Create a new field tab.
    pub fn new(label: impl Into<String>, fields: Vec<FieldDefinition>) -> Self {
        Self {
            label: label.into(),
            description: None,
            fields,
        }
    }
}

/// A block type definition for Blocks fields.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BlockDefinition {
    /// Unique identifier/slug for this block type.
    #[serde(default)]
    pub block_type: String,
    /// Fields contained within this block type.
    #[serde(default)]
    pub fields: Vec<FieldDefinition>,
    /// Localized display label for the block.
    #[serde(default)]
    pub label: Option<LocalizedString>,
    /// Sub-field name to use as row label for this block type.
    #[serde(default)]
    pub label_field: Option<String>,
    /// Group name for organizing blocks in the picker dropdown.
    #[serde(default)]
    pub group: Option<String>,
    /// Image URL for displaying an icon/thumbnail in the block picker.
    #[serde(default)]
    pub image_url: Option<String>,
}

impl BlockDefinition {
    /// Create a new block definition.
    pub fn new(block_type: impl Into<String>, fields: Vec<FieldDefinition>) -> Self {
        Self {
            block_type: block_type.into(),
            fields,
            ..Default::default()
        }
    }
}
