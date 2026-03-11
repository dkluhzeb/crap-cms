//! Admin UI display hints for fields.

use super::LocalizedString;
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

/// Admin UI display hints for a field (placeholder, description, visibility, width).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldAdmin {
    /// Localized display label for the field.
    #[serde(default)]
    pub label: Option<LocalizedString>,
    /// Localized placeholder text for inputs.
    #[serde(default)]
    pub placeholder: Option<LocalizedString>,
    /// Localized help text/description displayed below the field.
    #[serde(default)]
    pub description: Option<LocalizedString>,
    /// Whether the field is hidden from the admin UI.
    #[serde(default)]
    pub hidden: bool,
    /// Whether the field is read-only in the admin UI.
    #[serde(default)]
    pub readonly: bool,
    /// CSS width for the field container (e.g., "50%", "33%").
    #[serde(default)]
    pub width: Option<String>,
    /// Start collapsed in the admin UI (groups, collapsibles, array/block rows).
    #[serde(default = "default_true")]
    pub collapsed: bool,
    /// Sub-field name to use as row label (arrays/blocks).
    #[serde(default)]
    pub label_field: Option<String>,
    /// Lua function ref for computed row labels (arrays/blocks).
    #[serde(default)]
    pub row_label: Option<String>,
    /// Custom singular label for row items (e.g., "Slide" -> "Add Slide").
    #[serde(default)]
    pub labels_singular: Option<LocalizedString>,
    /// Custom plural label for the field header.
    #[serde(default)]
    pub labels_plural: Option<LocalizedString>,
    /// Field position in the admin form layout ("main" or "sidebar").
    /// Defaults to "main" when not set.
    #[serde(default)]
    pub position: Option<String>,
    /// Lua function ref for conditional field visibility.
    /// The function receives form data and returns either:
    /// - a boolean (server-evaluated on each change via HTMX)
    /// - a condition table (serialized to JSON, client-evaluated instantly)
    #[serde(default)]
    pub condition: Option<String>,
    /// For number fields: the step attribute on the input (e.g., "1", "0.01", "any").
    #[serde(default)]
    pub step: Option<String>,
    /// For textarea fields: number of visible rows (default 8).
    #[serde(default)]
    pub rows: Option<u32>,
    /// For code fields: the language mode (e.g., "json", "javascript", "html", "css", "python").
    #[serde(default)]
    pub language: Option<String>,
    /// For richtext fields: enabled toolbar features.
    /// When empty, all features are enabled. Possible values:
    /// "bold", "italic", "code", "link", "heading", "blockquote",
    /// "orderedList", "bulletList", "codeBlock", "horizontalRule".
    #[serde(default)]
    pub features: Vec<String>,
    /// For blocks fields: picker style. "select" (default) uses a dropdown,
    /// "card" uses a visual card grid (shows images when image_url is set on blocks).
    #[serde(default)]
    pub picker: Option<String>,
    /// For richtext fields: storage format. "html" (default) or "json" (ProseMirror JSON).
    #[serde(default)]
    pub richtext_format: Option<String>,
    /// For richtext fields: which custom node types are available.
    /// Node names must be registered via `crap.richtext.register_node()`.
    #[serde(default)]
    pub nodes: Vec<String>,
}

impl FieldAdmin {
    /// Returns a new `FieldAdminBuilder` for constructing display hints.
    pub fn builder() -> super::FieldAdminBuilder {
        super::FieldAdminBuilder::new()
    }
}

impl Default for FieldAdmin {
    fn default() -> Self {
        Self {
            label: None,
            placeholder: None,
            description: None,
            hidden: false,
            readonly: false,
            width: None,
            collapsed: true,
            label_field: None,
            row_label: None,
            labels_singular: None,
            labels_plural: None,
            position: None,
            condition: None,
            step: None,
            rows: None,
            language: None,
            features: Vec::new(),
            picker: None,
            richtext_format: None,
            nodes: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn richtext_format_default_is_none() {
        let admin = FieldAdmin::default();
        assert!(admin.richtext_format.is_none());
    }
}
