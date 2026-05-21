//! Block and tab definitions for Blocks/Tabs layout fields.

use crate::core::{FieldDefinition, LocalizedString};
use crate::typegen::lua::LuaAnnotation;
use serde::{Deserialize, Serialize};

/// A single tab within a Tabs layout field.
#[derive(Debug, Clone, Serialize, Deserialize, Default, LuaAnnotation)]
#[lua(class = "crap.FieldTab")]
pub struct FieldTab {
    /// Tab label displayed in the tab bar (required).
    #[serde(default)]
    pub label: String,
    /// Help text shown inside the tab panel.
    #[serde(default)]
    pub description: Option<String>,
    /// Fields within this tab.
    #[serde(default)]
    #[lua(ty = "crap.FieldDefinition[]")]
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
#[derive(Debug, Clone, Serialize, Deserialize, Default, LuaAnnotation)]
#[lua(class = "crap.BlockDefinition")]
pub struct BlockDefinition {
    /// Block type identifier (required).
    #[serde(default)]
    #[lua(rename = "type")]
    pub block_type: String,
    /// Fields within this block type.
    #[serde(default)]
    #[lua(ty = "crap.FieldDefinition[]")]
    pub fields: Vec<FieldDefinition>,
    /// Display label for the block type (defaults to type name).
    #[serde(default)]
    #[lua(ty = "crap.LocalizedString")]
    pub label: Option<LocalizedString>,
    /// Sub-field name to use as row label for this block type. Overrides the field-level `admin.label_field` for blocks of this type.
    #[serde(default)]
    pub label_field: Option<String>,
    /// Group name for organizing blocks in the picker dropdown (rendered as `<optgroup>`).
    #[serde(default)]
    pub group: Option<String>,
    /// Image URL for displaying an icon/thumbnail in the block picker. When any block has an image, the picker renders as a visual card grid.
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
