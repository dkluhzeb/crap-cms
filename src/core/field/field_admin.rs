//! Admin UI display hints for fields.

use crate::core::LocalizedString;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

fn default_true() -> bool {
    true
}

/// Admin UI display hints for a field (placeholder, description, visibility, width).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// For code fields: the default language mode (e.g., "json", "javascript", "html", "css", "python").
    /// When `languages` is non-empty, this is the initial value; the editor
    /// can switch to any other language in the allow-list at edit time.
    #[serde(default)]
    pub language: Option<String>,
    /// For code fields: an allow-list of languages the editor can pick from
    /// at edit time. When set, the form renders a `<select>` next to the
    /// editor and the editor's choice persists in a `<name>_lang` companion
    /// column. When empty, the language is fixed to `language` (or `"json"`
    /// if neither is set).
    #[serde(default)]
    pub languages: Vec<String>,
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
    /// Allow vertical resize on textarea/richtext fields (default: true).
    #[serde(default = "default_true")]
    pub resizable: bool,
    /// Custom template name to render this field instead of the default
    /// `fields/<field_type>` lookup. The path is relative to `templates/`
    /// (no `.hbs` extension), e.g. `"fields/rating"` or
    /// `"fields/custom/star-picker"`. Provides a per-instance opt-out from
    /// the type-based template routing in `RenderFieldHelper` so an
    /// individual field can declare its own admin UI without requiring a
    /// global override of `templates/fields/<type>.hbs`. The custom
    /// template receives the same field render context as the built-in
    /// template at that path. Restricted to safe characters
    /// (`a-zA-Z0-9/_-`); `..` and absolute paths are rejected.
    #[serde(default)]
    pub template: Option<String>,
    /// Freeform per-field configuration map, available to the field's
    /// admin template at `{{admin.extra.<key>}}`. Pairs naturally with
    /// [`Self::template`] — a custom template reads its config from
    /// `extra` so the same template + JS component can be reused across
    /// fields with different settings (icon, color, swatches,
    /// step labels, …) without per-field forking.
    ///
    /// Values are JSON-serializable (string / number / bool / array /
    /// nested object). For dynamic values (computed at render time),
    /// register a [`crap.template_data`](crate::admin::templates) function
    /// and pull from `{{data "name"}}` in the template instead — `extra`
    /// is parsed once at field-definition time and is **static** per
    /// field instance.
    ///
    /// Empty by default. Empty maps don't serialize (`skip_serializing_if`),
    /// keeping schema dumps and roundtrips clean.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
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
            languages: Vec::new(),
            features: Vec::new(),
            picker: None,
            richtext_format: None,
            nodes: Vec::new(),
            resizable: true,
            template: None,
            extra: Map::new(),
        }
    }
}

/// Validate a template-name path supplied by user config (`admin.template`).
///
/// Rejects empty strings, absolute paths (`/...`), parent-dir traversal
/// (`..`), and any character outside `[a-zA-Z0-9/_-]`. The resulting
/// path is resolved by the Handlebars template registry as a logical
/// name (no filesystem lookup beyond what `register_template_string`
/// produces), but applying the same character-whitelist as user-facing
/// path inputs prevents injection of unexpected names.
pub fn validate_template_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("template name must not be empty".to_string());
    }
    if name.starts_with('/') {
        return Err(format!(
            "template name `{name}` must not start with `/` (use a path relative to templates/)"
        ));
    }
    if name.ends_with('/') {
        return Err(format!("template name `{name}` must not end with `/`"));
    }
    if name.contains("//") {
        return Err(format!(
            "template name `{name}` must not contain empty path segments (`//`)"
        ));
    }
    if name
        .split('/')
        .any(|segment| segment == ".." || segment == ".")
    {
        return Err(format!(
            "template name `{name}` must not contain `..` or `.` segments"
        ));
    }
    for c in name.chars() {
        let ok = c.is_ascii_alphanumeric() || c == '/' || c == '_' || c == '-';
        if !ok {
            return Err(format!(
                "template name `{name}` contains invalid character `{c}` (allowed: a-z, A-Z, 0-9, /, _, -)"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn richtext_format_default_is_none() {
        let admin = FieldAdmin::default();
        assert!(admin.richtext_format.is_none());
    }

    #[test]
    fn resizable_defaults_to_true() {
        let admin = FieldAdmin::default();
        assert!(admin.resizable);
    }

    #[test]
    fn template_defaults_to_none() {
        let admin = FieldAdmin::default();
        assert!(admin.template.is_none());
    }

    #[test]
    fn extra_defaults_to_empty_and_does_not_serialize() {
        let admin = FieldAdmin::default();
        assert!(admin.extra.is_empty());
        let json = serde_json::to_value(&admin).unwrap();
        // skip_serializing_if = "Map::is_empty" keeps the field out
        // when nothing is set, so schema dumps / roundtrips stay clean.
        assert!(
            json.get("extra").is_none(),
            "empty extra should not serialize, got: {json}"
        );
    }

    #[test]
    fn template_name_accepts_safe_paths() {
        for ok in [
            "fields/rating",
            "fields/custom/star-picker",
            "fields/acme_v2/rating",
            "x",
            "deep/nested/path-with_chars",
        ] {
            assert!(
                validate_template_name(ok).is_ok(),
                "expected `{ok}` to be valid"
            );
        }
    }

    #[test]
    fn template_name_rejects_unsafe_paths() {
        for bad in [
            "",                     // empty
            "/fields/rating",       // absolute
            "fields/rating/",       // trailing slash
            "fields//rating",       // empty segment
            "fields/",              // bare trailing slash
            "../../etc/passwd",     // traversal
            "fields/../leaked",     // mid-path traversal
            "fields/./rating",      // current-dir
            "fields/with space",    // space
            "fields/with;semi",     // shell metachar
            "fields/with$var",      // dollar
            "fields/with\nnewline", // newline
            "fields/with\0null",    // NULL byte
            "fields/with\\back",    // backslash
            "fields/with%enc",      // percent (encode-attack vector)
        ] {
            assert!(
                validate_template_name(bad).is_err(),
                "expected `{bad}` to be rejected, got Ok",
            );
        }
    }
}
