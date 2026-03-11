//! Block and tab definitions for Blocks/Tabs layout fields.

use super::{FieldDefinition, LocalizedString};
use serde::{Deserialize, Serialize};

/// A single tab within a Tabs layout field.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FieldTab {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub fields: Vec<FieldDefinition>,
}

impl FieldTab {
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
    #[serde(default)]
    pub block_type: String,
    #[serde(default)]
    pub fields: Vec<FieldDefinition>,
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
    pub fn new(block_type: impl Into<String>, fields: Vec<FieldDefinition>) -> Self {
        Self {
            block_type: block_type.into(),
            fields,
            ..Default::default()
        }
    }
}
