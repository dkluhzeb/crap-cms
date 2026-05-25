//! Admin UI display hints for fields.

use crate::core::LocalizedString;
use crate::typegen::lua::{LuaAlias, LuaAnnotation};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

fn default_true() -> bool {
    true
}

/// Field width on the admin form. The three named variants line up
/// with the `form__field--*` CSS classes; `Custom` accepts any CSS
/// width value (e.g. `"50%"`, `"200px"`) for cases the presets don't
/// cover — passed through unchanged to the rendered field's
/// `style.width`.
#[derive(Debug, Clone, PartialEq, Eq, LuaAlias)]
#[lua(alias = "crap.FieldWidth", rename_all = "lowercase")]
pub enum FieldWidth {
    /// Full row width (default).
    Full,
    /// Half row width.
    Half,
    /// One-third row width.
    Third,
    /// Arbitrary CSS width string (`"50%"`, `"200px"`, etc.).
    Custom(String),
}

impl FieldWidth {
    /// Return the canonical Lua-side string for this width.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Full => "full",
            Self::Half => "half",
            Self::Third => "third",
            Self::Custom(s) => s,
        }
    }
}

impl From<String> for FieldWidth {
    fn from(s: String) -> Self {
        match s.as_str() {
            "full" => Self::Full,
            "half" => Self::Half,
            "third" => Self::Third,
            _ => Self::Custom(s),
        }
    }
}

impl From<&str> for FieldWidth {
    fn from(s: &str) -> Self {
        Self::from(s.to_string())
    }
}

impl Serialize for FieldWidth {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FieldWidth {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Ok(Self::from(s))
    }
}

/// Custom singular/plural labels for row items (arrays/blocks).
//
// Surfaced in Lua as `admin.labels = { singular = ..., plural = ... }`.
// Either side may be `None`; an empty struct serializes to `{}` and is
// skipped at the `admin` level via `FieldAdminLabels::is_empty`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, LuaAnnotation)]
#[serde(default)]
#[lua(class = "crap.FieldAdminLabels")]
pub struct FieldAdminLabels {
    /// Custom singular label for row items (e.g., "Slide" → "Add Slide" button).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "crap.LocalizedString", optional)]
    pub singular: Option<LocalizedString>,
    /// Custom plural label for the field header.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "crap.LocalizedString", optional)]
    pub plural: Option<LocalizedString>,
}

impl FieldAdminLabels {
    /// True iff both `singular` and `plural` are `None` — the default,
    /// "no overrides" state. Used by `FieldAdmin` to skip serialization
    /// when nothing is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.singular.is_none() && self.plural.is_none()
    }
}

/// Admin UI display hints for a field (placeholder, description, visibility, width).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.FieldAdmin")]
pub struct FieldAdmin {
    /// UI label (defaults to field name).
    #[serde(default)]
    #[lua(ty = "crap.LocalizedString", optional)]
    pub label: Option<LocalizedString>,
    /// Input placeholder text.
    #[serde(default)]
    #[lua(ty = "crap.LocalizedString", optional)]
    pub placeholder: Option<LocalizedString>,
    /// Help text shown below the input.
    #[serde(default)]
    #[lua(ty = "crap.LocalizedString", optional)]
    pub description: Option<LocalizedString>,
    /// Hide from the admin edit form only (default: false). The field's value is still returned in API responses (gRPC, Lua, MCP, REST). For full API stripping, use top-level `hidden` on `crap.FieldDefinition` instead.
    #[serde(default)]
    #[lua(optional)]
    pub hidden: bool,
    /// Non-editable in admin (default: false).
    #[serde(default)]
    #[lua(optional)]
    pub readonly: bool,
    /// Field width: `"full"`, `"half"`, `"third"`, or an arbitrary CSS
    /// value (e.g. `"50%"`, `"200px"`).
    #[serde(default)]
    #[lua(ty = "crap.FieldWidth", optional)]
    pub width: Option<FieldWidth>,
    /// Start collapsed in admin UI — groups, collapsibles, array/block rows (default: true). Set `false` to start expanded.
    #[serde(default = "default_true")]
    #[lua(optional)]
    pub collapsed: bool,
    /// Sub-field name to use as row label in admin (arrays/blocks). The value of this sub-field is shown as the row title. For blocks, per-block `label_field` on `BlockDefinition` takes priority.
    #[serde(default)]
    #[lua(optional)]
    pub label_field: Option<String>,
    /// Lua function ref for computed row labels (arrays/blocks). Receives the row data table, returns a display string or nil. Takes priority over `label_field`. Signature: `fun(row: table): string?`.
    #[serde(default)]
    #[lua(optional)]
    pub row_label: Option<String>,
    /// Custom singular/plural labels for row items (e.g., `{ singular = "Slide", plural = "Slides" }` → "Add Slide" button).
    #[serde(default, skip_serializing_if = "FieldAdminLabels::is_empty")]
    #[lua(optional)]
    pub labels: FieldAdminLabels,
    /// "main" or "sidebar".
    #[serde(default)]
    #[lua(optional)]
    pub position: Option<String>,
    /// Lua function ref (string) for conditional show/hide — e.g.
    /// `"hooks.conditions.show_external_url"`. Inline condition tables
    /// are not accepted here; this field is the name of the function,
    /// not the condition itself.
    ///
    /// The referenced function receives form data and returns either:
    /// - a boolean (server-evaluated on each change via HTMX), or
    /// - a condition table (serialized to JSON, client-evaluated instantly).
    ///
    /// See `docs/src/admin-ui/guides/display-conditions.md`.
    #[serde(default)]
    #[lua(optional)]
    pub condition: Option<String>,
    /// Step value for number inputs (default: "any"). Use "1" for integers, "0.01" for cents, etc.
    #[serde(default)]
    #[lua(optional)]
    pub step: Option<String>,
    /// Number of rows for textarea fields (default: 8).
    #[serde(default)]
    #[lua(optional)]
    pub rows: Option<u32>,
    /// Default language mode for code fields (default: "json"). Options: "json", "javascript", "html", "css", "python", "plain".
    ///
    /// When `languages` is non-empty, this is the initial value; the editor
    /// can switch to any other language in the allow-list at edit time.
    #[serde(default)]
    #[lua(optional)]
    pub language: Option<String>,
    /// Allow-list of languages the editor can pick from at edit time for code fields. When set, the form renders a `<select>` next to the editor and persists the choice in a `<name>_lang` companion column. When empty/absent, the language is fixed to `language`.
    #[serde(default)]
    #[lua(optional)]
    pub languages: Vec<String>,
    /// Enabled toolbar features for richtext fields. When absent, all features are enabled. Options: "bold", "italic", "code", "link", "heading", "blockquote", "orderedList", "bulletList", "codeBlock", "horizontalRule".
    #[serde(default)]
    #[lua(optional)]
    pub features: Vec<String>,
    /// Picker UI style. For blocks fields: "select" (default) uses a dropdown, "card" uses a visual card grid. For upload fields: "drawer" (default) adds a browse button with thumbnail grid, "none" disables it. For relationship fields: "drawer" adds a browse button with searchable list.
    #[serde(default)]
    #[lua(optional)]
    pub picker: Option<String>,
    /// Storage format for richtext fields: "html" (default) or "json" (`ProseMirror` JSON). FTS extracts plain text from JSON automatically.
    #[serde(default)]
    #[lua(rename = "format", optional)]
    pub richtext_format: Option<String>,
    /// Custom `ProseMirror` node types for richtext fields. Names must match nodes registered via `crap.richtext.register_node()`.
    #[serde(default)]
    #[lua(optional)]
    pub nodes: Vec<String>,
    /// Allow vertical resize on textarea/richtext fields (default: true).
    #[serde(default = "default_true")]
    #[lua(optional)]
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
    #[lua(optional)]
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
    #[lua(ty = "table<string, any>", optional)]
    pub extra: Map<String, Value>,
}

impl FieldAdmin {
    /// Returns a new `FieldAdminBuilder` for constructing display hints.
    #[must_use]
    pub fn builder() -> FieldAdminBuilder {
        FieldAdminBuilder::new()
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
            labels: FieldAdminLabels::default(),
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
///
/// # Errors
///
/// Returns a descriptive error string if the name is empty, absolute,
/// contains `..`, or includes characters outside `[a-zA-Z0-9/_-]`.
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

/// Builder for [`FieldAdmin`].
///
/// All fields are optional and default via [`FieldAdmin::default()`].
pub struct FieldAdminBuilder {
    inner: FieldAdmin,
}

impl Default for FieldAdminBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl FieldAdminBuilder {
    /// Create a new `FieldAdminBuilder`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: FieldAdmin::default(),
        }
    }

    /// Set the localized label for the field.
    #[must_use]
    pub fn label(mut self, v: LocalizedString) -> Self {
        self.inner.label = Some(v);
        self
    }

    /// Set the localized placeholder text for the field.
    #[must_use]
    pub fn placeholder(mut self, v: LocalizedString) -> Self {
        self.inner.placeholder = Some(v);
        self
    }

    /// Set the localized description text for the field.
    #[must_use]
    pub fn description(mut self, v: LocalizedString) -> Self {
        self.inner.description = Some(v);
        self
    }

    /// Set whether the field is hidden in the admin UI.
    #[must_use]
    pub fn hidden(mut self, v: bool) -> Self {
        self.inner.hidden = v;
        self
    }

    /// Set whether the field is read-only in the admin UI.
    #[must_use]
    pub fn readonly(mut self, v: bool) -> Self {
        self.inner.readonly = v;
        self
    }

    /// Set the field width — `"full"`, `"half"`, `"third"`, or any CSS
    /// value (e.g. `"50%"`).
    #[must_use]
    pub fn width(mut self, v: impl Into<FieldWidth>) -> Self {
        self.inner.width = Some(v.into());
        self
    }

    /// Set whether the field starts collapsed.
    #[must_use]
    pub fn collapsed(mut self, v: bool) -> Self {
        self.inner.collapsed = v;
        self
    }

    /// Set the sub-field name to use as row label (for arrays/blocks).
    #[must_use]
    pub fn label_field(mut self, v: impl Into<String>) -> Self {
        self.inner.label_field = Some(v.into());
        self
    }

    /// Set the Lua function ref for computed row labels.
    #[must_use]
    pub fn row_label(mut self, v: impl Into<String>) -> Self {
        self.inner.row_label = Some(v.into());
        self
    }

    /// Set custom singular label for row items.
    #[must_use]
    pub fn labels_singular(mut self, v: LocalizedString) -> Self {
        self.inner.labels.singular = Some(v);
        self
    }

    /// Set custom plural label for the field.
    #[must_use]
    pub fn labels_plural(mut self, v: LocalizedString) -> Self {
        self.inner.labels.plural = Some(v);
        self
    }

    /// Set both singular and plural row-label overrides from a built
    /// [`FieldAdminLabels`]. Use [`Self::labels_singular`] /
    /// [`Self::labels_plural`] for one-off setters.
    #[must_use]
    pub fn labels(mut self, v: FieldAdminLabels) -> Self {
        self.inner.labels = v;
        self
    }

    /// Set field position in layout ("main" or "sidebar").
    #[must_use]
    pub fn position(mut self, v: impl Into<String>) -> Self {
        self.inner.position = Some(v.into());
        self
    }

    /// Set Lua function ref for conditional visibility.
    #[must_use]
    pub fn condition(mut self, v: impl Into<String>) -> Self {
        self.inner.condition = Some(v.into());
        self
    }

    /// Set step attribute for number inputs.
    #[must_use]
    pub fn step(mut self, v: impl Into<String>) -> Self {
        self.inner.step = Some(v.into());
        self
    }

    /// Set number of visible rows for textarea fields.
    #[must_use]
    pub fn rows(mut self, v: u32) -> Self {
        self.inner.rows = Some(v);
        self
    }

    /// Set language mode for code fields.
    #[must_use]
    pub fn language(mut self, v: impl Into<String>) -> Self {
        self.inner.language = Some(v.into());
        self
    }

    /// Set the allow-list of languages the editor can pick from at edit time
    /// for code fields. When non-empty, the form renders a language picker.
    #[must_use]
    pub fn languages(mut self, v: Vec<String>) -> Self {
        self.inner.languages = v;
        self
    }

    /// Set enabled toolbar features for richtext fields.
    #[must_use]
    pub fn features(mut self, v: Vec<String>) -> Self {
        self.inner.features = v;
        self
    }

    /// Set picker style for blocks fields ("select" or "card").
    #[must_use]
    pub fn picker(mut self, v: impl Into<String>) -> Self {
        self.inner.picker = Some(v.into());
        self
    }

    /// Set storage format for richtext fields ("html" or "json").
    #[must_use]
    pub fn richtext_format(mut self, v: impl Into<String>) -> Self {
        self.inner.richtext_format = Some(v.into());
        self
    }

    /// Set available custom node types for richtext fields.
    #[must_use]
    pub fn nodes(mut self, v: Vec<String>) -> Self {
        self.inner.nodes = v;
        self
    }

    /// Set whether the field allows vertical resizing (textarea/richtext).
    #[must_use]
    pub fn resizable(mut self, v: bool) -> Self {
        self.inner.resizable = v;
        self
    }

    /// Set a custom template path that overrides the default
    /// `fields/<field_type>` lookup for this field instance. See
    /// [`FieldAdmin::template`] for details. The path is **not**
    /// validated by the builder — call
    /// [`crate::core::validate_template_name`] before passing user
    /// input.
    #[must_use]
    pub fn template(mut self, v: impl Into<String>) -> Self {
        self.inner.template = Some(v.into());
        self
    }

    /// Set the freeform `extra` map of per-field configuration. See
    /// [`FieldAdmin::extra`] for details. Replaces any previous extras.
    #[must_use]
    pub fn extra(mut self, v: Map<String, Value>) -> Self {
        self.inner.extra = v;
        self
    }

    /// Insert a single key into the freeform `extra` map. Convenience
    /// over building a full `Map` for the common case of attaching a
    /// handful of named values.
    #[must_use]
    pub fn extra_insert(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.inner.extra.insert(key.into(), value.into());
        self
    }

    /// Build the final `FieldAdmin`.
    #[must_use]
    pub fn build(self) -> FieldAdmin {
        self.inner
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

    #[test]
    fn builds_field_admin_with_defaults() {
        let admin = FieldAdminBuilder::new().build();
        assert!(admin.label.is_none());
        assert!(!admin.hidden);
        assert!(!admin.readonly);
        assert!(admin.collapsed);
        assert!(admin.features.is_empty());
        assert!(admin.resizable);
    }

    // ── FieldWidth round-trip ───────────────────────────────────────

    #[test]
    fn field_width_named_strings_map_to_named_variants() {
        assert!(matches!(
            FieldWidth::from("full".to_string()),
            FieldWidth::Full
        ));
        assert!(matches!(FieldWidth::from("half"), FieldWidth::Half));
        assert!(matches!(FieldWidth::from("third"), FieldWidth::Third));
    }

    #[test]
    fn field_width_unknown_string_becomes_custom() {
        let w = FieldWidth::from("50%");
        assert!(matches!(w, FieldWidth::Custom(ref s) if s == "50%"));
        assert_eq!(w.as_str(), "50%");
    }

    #[test]
    fn field_width_named_as_str_is_canonical() {
        assert_eq!(FieldWidth::Full.as_str(), "full");
        assert_eq!(FieldWidth::Half.as_str(), "half");
        assert_eq!(FieldWidth::Third.as_str(), "third");
    }

    #[test]
    fn field_width_serde_round_trips_named_and_custom() {
        // Named variant serializes to the lowercase string.
        let json = serde_json::to_string(&FieldWidth::Half).unwrap();
        assert_eq!(json, "\"half\"");
        let back: FieldWidth = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, FieldWidth::Half));

        // Custom variant serializes to the CSS string verbatim.
        let json = serde_json::to_string(&FieldWidth::Custom("200px".into())).unwrap();
        assert_eq!(json, "\"200px\"");
        let back: FieldWidth = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, FieldWidth::Custom(ref s) if s == "200px"));
    }

    #[test]
    fn builds_field_admin_with_overrides() {
        let admin = FieldAdminBuilder::new()
            .label(LocalizedString::Plain("Title".into()))
            .hidden(true)
            .readonly(true)
            .width("50%")
            .collapsed(false)
            .position("sidebar")
            .rows(12)
            .resizable(false)
            .build();
        assert!(admin.label.is_some());
        assert!(admin.hidden);
        assert!(admin.readonly);
        assert_eq!(admin.width.as_ref().map(FieldWidth::as_str), Some("50%"));
        assert!(matches!(admin.width, Some(FieldWidth::Custom(ref s)) if s == "50%"));
        assert!(!admin.collapsed);
        assert_eq!(admin.position.as_deref(), Some("sidebar"));
        assert_eq!(admin.rows, Some(12));
        assert!(!admin.resizable);
    }

    #[test]
    fn builds_field_admin_with_richtext_options() {
        let admin = FieldAdminBuilder::new()
            .richtext_format("json")
            .features(vec!["bold".into(), "italic".into()])
            .nodes(vec!["cta".into()])
            .build();
        assert_eq!(admin.richtext_format.as_deref(), Some("json"));
        assert_eq!(admin.features.len(), 2);
        assert_eq!(admin.nodes, vec!["cta"]);
    }
}
