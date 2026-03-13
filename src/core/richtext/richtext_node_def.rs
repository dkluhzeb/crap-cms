use serde::{Deserialize, Serialize};

use super::{node_attr::NodeAttr, richtext_node_def_builder::RichtextNodeDefBuilder};

/// A registered custom ProseMirror node type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RichtextNodeDef {
    pub name: String,
    pub label: String,
    pub inline: bool,
    pub attrs: Vec<NodeAttr>,
    /// Which attrs contain searchable text (for FTS extraction).
    #[serde(default)]
    pub searchable_attrs: Vec<String>,
    /// Whether a Lua render function exists (the function itself lives in the Lua VM).
    #[serde(default)]
    pub has_render: bool,
}

impl RichtextNodeDef {
    pub fn builder(name: impl Into<String>, label: impl Into<String>) -> RichtextNodeDefBuilder {
        RichtextNodeDefBuilder::new(name, label)
    }
}
