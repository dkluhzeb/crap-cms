//! Block and tab definitions for Blocks/Tabs layout fields.

use crate::core::{FieldDefinition, LocalizedString, field::to_title_case};
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

    /// The block's display label: the explicit `label` if set, else the
    /// `block_type` slug title-cased. One source so the admin editor and the
    /// DB-read walkers (back-references, missing-relations) can't render the same
    /// unlabeled block two different ways (`hero_banner` vs `Hero Banner`).
    #[must_use]
    pub fn display_label(&self) -> String {
        self.label.as_ref().map_or_else(
            || to_title_case(&self.block_type),
            |ls| ls.resolve_default().to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::FieldType;

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    #[test]
    fn field_tab_new_sets_label_and_fields_without_description() {
        let tab = FieldTab::new("SEO", vec![text("title")]);
        assert_eq!(tab.label, "SEO");
        assert!(tab.description.is_none());
        assert_eq!(tab.fields.len(), 1);
        assert_eq!(tab.fields[0].name, "title");
    }

    #[test]
    fn block_definition_new_sets_type_and_fields_defaulting_the_rest() {
        let block = BlockDefinition::new("hero", vec![text("heading")]);
        assert_eq!(block.block_type, "hero");
        assert_eq!(block.fields.len(), 1);
        assert_eq!(block.fields[0].name, "heading");
        // Optional presentation fields must default to None.
        assert!(block.label.is_none());
        assert!(block.label_field.is_none());
        assert!(block.group.is_none());
        assert!(block.image_url.is_none());
    }
}
