//! Custom `ProseMirror` node type registered from Lua, plus its builder.

use serde::{Deserialize, Serialize};

use crate::core::{Builder, FieldDefinition};

/// A registered custom `ProseMirror` node type.
#[derive(Debug, Clone, Serialize, Deserialize, Builder)]
pub struct RichtextNodeDef {
    #[builder(required)]
    pub name: String,
    #[builder(required)]
    pub label: String,
    pub inline: bool,
    pub attrs: Vec<FieldDefinition>,
    /// Which attrs contain searchable text (for FTS extraction).
    #[serde(default)]
    pub searchable_attrs: Vec<String>,
    /// Whether a Lua render function exists (the function itself lives in the Lua VM).
    #[serde(default)]
    pub has_render: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::FieldType;

    #[test]
    fn builds_richtext_node_def_with_defaults() {
        let def = RichtextNodeDef::builder("cta", "Call to Action").build();
        assert_eq!(def.name, "cta");
        assert_eq!(def.label, "Call to Action");
        assert!(!def.inline);
        assert!(def.attrs.is_empty());
        assert!(def.searchable_attrs.is_empty());
        assert!(!def.has_render);
    }

    #[test]
    fn builds_richtext_node_def_with_overrides() {
        let attr = FieldDefinition::builder("url", FieldType::Text).build();
        let def = RichtextNodeDef::builder("embed", "Embed")
            .inline(true)
            .attrs(vec![attr])
            .searchable_attrs(vec!["caption".to_string()])
            .has_render(true)
            .build();
        assert!(def.inline);
        assert_eq!(def.attrs.len(), 1);
        assert_eq!(def.searchable_attrs, vec!["caption"]);
        assert!(def.has_render);
    }
}
