//! Custom `ProseMirror` node type registered from Lua, plus its builder.

use serde::{Deserialize, Serialize};

use crate::core::FieldDefinition;

/// A registered custom `ProseMirror` node type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RichtextNodeDef {
    pub name: String,
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

impl RichtextNodeDef {
    pub fn builder(name: impl Into<String>, label: impl Into<String>) -> RichtextNodeDefBuilder {
        RichtextNodeDefBuilder::new(name, label)
    }
}

/// Builder for [`RichtextNodeDef`].
///
/// `name` and `label` are taken in `new()`. Sensible defaults are pre-populated.
pub struct RichtextNodeDefBuilder {
    name: String,
    label: String,
    inline: bool,
    attrs: Vec<FieldDefinition>,
    searchable_attrs: Vec<String>,
    has_render: bool,
}

impl RichtextNodeDefBuilder {
    pub fn new(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            inline: false,
            attrs: Vec::new(),
            searchable_attrs: Vec::new(),
            has_render: false,
        }
    }

    #[must_use]
    pub fn inline(mut self, b: bool) -> Self {
        self.inline = b;
        self
    }

    #[must_use]
    pub fn attrs(mut self, a: Vec<FieldDefinition>) -> Self {
        self.attrs = a;
        self
    }

    #[must_use]
    pub fn searchable_attrs(mut self, s: Vec<String>) -> Self {
        self.searchable_attrs = s;
        self
    }

    #[must_use]
    pub fn has_render(mut self, b: bool) -> Self {
        self.has_render = b;
        self
    }

    #[must_use]
    pub fn build(self) -> RichtextNodeDef {
        RichtextNodeDef {
            name: self.name,
            label: self.label,
            inline: self.inline,
            attrs: self.attrs,
            searchable_attrs: self.searchable_attrs,
            has_render: self.has_render,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::FieldType;

    #[test]
    fn builds_richtext_node_def_with_defaults() {
        let def = RichtextNodeDefBuilder::new("cta", "Call to Action").build();
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
        let def = RichtextNodeDefBuilder::new("embed", "Embed")
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
