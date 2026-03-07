//! Field types and definitions. Each field maps to a column (or join table) in SQLite.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A string that can be plain or per-locale.
/// Plain: `"Title"` — works like before.
/// Localized: `{ en = "Title", de = "Titel" }` — resolved at render time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LocalizedString {
    Plain(String),
    Localized(HashMap<String, String>),
}

impl LocalizedString {
    /// Resolve to a single string for the given locale, with fallback to default locale.
    pub fn resolve(&self, locale: &str, default_locale: &str) -> &str {
        match self {
            LocalizedString::Plain(s) => s,
            LocalizedString::Localized(map) => {
                map.get(locale)
                    .or_else(|| map.get(default_locale))
                    .map(|s| s.as_str())
                    .unwrap_or("")
            }
        }
    }

    /// Resolve using the default locale only (for when locale config is disabled).
    pub fn resolve_default(&self) -> &str {
        match self {
            LocalizedString::Plain(s) => s,
            LocalizedString::Localized(map) => {
                map.values().next().map(|s| s.as_str()).unwrap_or("")
            }
        }
    }
}

/// Supported field types. Each variant maps to a SQLite column type (or join table for Array/Blocks/has-many).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    Text,
    Number,
    Textarea,
    Select,
    Checkbox,
    Date,
    Email,
    Json,
    Richtext,
    Relationship,
    Array,
    Group,
    Upload,
    Blocks,
    /// Radio buttons. Same as Select in storage (TEXT column) but renders as radio group.
    Radio,
    /// Layout-only row container. Sub-fields are promoted to parent-level columns
    /// (no prefix, unlike Group). Used only for horizontal layout in the admin UI.
    Row,
    /// Layout-only collapsible container. Sub-fields are promoted to parent-level columns
    /// (no prefix, like Row). Used for a collapsible section in the admin UI.
    Collapsible,
    /// Layout-only tabbed container. Sub-fields (across all tabs) are promoted to
    /// parent-level columns (no prefix, like Row). Used for tabbed sections in the admin UI.
    Tabs,
    /// Code editor field. Renders a CodeMirror editor in the admin UI.
    /// Stored as plain TEXT in SQLite.
    Code,
    /// Virtual reverse-relationship field. Shows documents from another collection
    /// that reference this document. No stored data — computed at read time.
    Join,
}

/// MCP-specific configuration for a field.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct McpFieldConfig {
    /// Description used in MCP tool JSON Schema for this field.
    pub description: Option<String>,
}

/// Configuration for join (virtual reverse-relationship) fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinConfig {
    /// Target collection slug (the collection whose documents reference this one).
    pub collection: String,
    /// Field name on the target collection that holds this document's ID.
    pub on: String,
}

/// Configuration for relationship fields (target collection, cardinality, depth cap).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipConfig {
    pub collection: String,
    pub has_many: bool,
    /// Per-field max depth. If set, limits population depth for this field
    /// regardless of the request-level depth.
    #[serde(default)]
    pub max_depth: Option<i32>,
    /// Polymorphic relationship: additional target collections beyond `collection`.
    /// Empty = single-collection relationship (default, backward compat).
    /// Non-empty = polymorphic (all targets listed here, `collection` = first).
    #[serde(default)]
    pub polymorphic: Vec<String>,
}

impl RelationshipConfig {
    /// Returns true if this relationship targets multiple collections.
    pub fn is_polymorphic(&self) -> bool {
        !self.polymorphic.is_empty()
    }

    /// Returns all target collections (polymorphic list, or single `collection`).
    pub fn all_collections(&self) -> Vec<&str> {
        if self.is_polymorphic() {
            self.polymorphic.iter().map(|s| s.as_str()).collect()
        } else {
            vec![self.collection.as_str()]
        }
    }
}

impl FieldType {
    pub fn sqlite_type(&self) -> &'static str {
        match self {
            FieldType::Text => "TEXT",
            FieldType::Number => "REAL",
            FieldType::Textarea => "TEXT",
            FieldType::Select => "TEXT",
            FieldType::Radio => "TEXT",
            FieldType::Checkbox => "INTEGER",
            FieldType::Date => "TEXT",
            FieldType::Email => "TEXT",
            FieldType::Json => "TEXT",
            FieldType::Richtext => "TEXT",
            FieldType::Relationship => "TEXT",
            FieldType::Array => "TEXT", // never used — arrays use join tables
            FieldType::Group => "TEXT", // never used — sub-fields get prefixed columns
            FieldType::Upload => "TEXT",
            FieldType::Blocks => "TEXT", // never used — blocks use join tables
            FieldType::Row => "TEXT", // never used — sub-fields are promoted to parent
            FieldType::Collapsible => "TEXT", // never used — sub-fields are promoted to parent
            FieldType::Tabs => "TEXT", // never used — sub-fields are promoted to parent
            FieldType::Code => "TEXT",
            FieldType::Join => "TEXT", // never used — virtual field, no column
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "text" => FieldType::Text,
            "number" => FieldType::Number,
            "textarea" => FieldType::Textarea,
            "select" => FieldType::Select,
            "radio" => FieldType::Radio,
            "checkbox" => FieldType::Checkbox,
            "date" => FieldType::Date,
            "email" => FieldType::Email,
            "json" => FieldType::Json,
            "richtext" => FieldType::Richtext,
            "relationship" => FieldType::Relationship,
            "array" => FieldType::Array,
            "group" => FieldType::Group,
            "upload" => FieldType::Upload,
            "blocks" => FieldType::Blocks,
            "row" => FieldType::Row,
            "collapsible" => FieldType::Collapsible,
            "tabs" => FieldType::Tabs,
            "code" => FieldType::Code,
            "join" => FieldType::Join,
            other => {
                tracing::warn!("Unknown field type '{}', defaulting to Text", other);
                FieldType::Text
            }
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            FieldType::Text => "text",
            FieldType::Number => "number",
            FieldType::Textarea => "textarea",
            FieldType::Select => "select",
            FieldType::Radio => "radio",
            FieldType::Checkbox => "checkbox",
            FieldType::Date => "date",
            FieldType::Email => "email",
            FieldType::Json => "json",
            FieldType::Richtext => "richtext",
            FieldType::Relationship => "relationship",
            FieldType::Array => "array",
            FieldType::Group => "group",
            FieldType::Upload => "upload",
            FieldType::Blocks => "blocks",
            FieldType::Row => "row",
            FieldType::Collapsible => "collapsible",
            FieldType::Tabs => "tabs",
            FieldType::Code => "code",
            FieldType::Join => "join",
        }
    }
}

/// A label/value pair for select field options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectOption {
    pub label: LocalizedString,
    pub value: String,
}

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

/// Admin UI display hints for a field (placeholder, description, visibility, width).
fn default_true() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldAdmin {
    #[serde(default)]
    pub label: Option<LocalizedString>,
    #[serde(default)]
    pub placeholder: Option<LocalizedString>,
    #[serde(default)]
    pub description: Option<LocalizedString>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub readonly: bool,
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

/// Lua function references for field-level access control (read/create/update).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FieldAccess {
    #[serde(default)]
    pub read: Option<String>,
    #[serde(default)]
    pub create: Option<String>,
    #[serde(default)]
    pub update: Option<String>,
}

/// Lua function references for field-level lifecycle hooks.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FieldHooks {
    #[serde(default)]
    pub before_validate: Vec<String>,
    #[serde(default)]
    pub before_change: Vec<String>,
    #[serde(default)]
    pub after_change: Vec<String>,
    #[serde(default)]
    pub after_read: Vec<String>,
}

impl FieldHooks {
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.before_validate.is_empty()
            && self.before_change.is_empty()
            && self.after_change.is_empty()
            && self.after_read.is_empty()
    }
}

/// Complete definition of a single field within a collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDefinition {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub unique: bool,
    #[serde(default)]
    pub index: bool,
    #[serde(default)]
    pub validate: Option<String>,
    #[serde(default)]
    pub default_value: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Vec<SelectOption>,
    #[serde(default)]
    pub admin: FieldAdmin,
    #[serde(default)]
    pub hooks: FieldHooks,
    #[serde(default)]
    pub access: FieldAccess,
    #[serde(default)]
    pub mcp: McpFieldConfig,
    #[serde(default)]
    pub relationship: Option<RelationshipConfig>,
    #[serde(default)]
    pub fields: Vec<FieldDefinition>,
    #[serde(default)]
    pub blocks: Vec<BlockDefinition>,
    #[serde(default)]
    pub tabs: Vec<FieldTab>,
    #[serde(default)]
    pub localized: bool,
    /// For date fields: controls the HTML input type and storage format.
    /// Valid values: "dayOnly" (default), "dayAndTime", "timeOnly", "monthOnly".
    #[serde(default)]
    pub picker_appearance: Option<String>,
    /// Minimum number of rows (array/blocks). Validated on create/update.
    #[serde(default)]
    pub min_rows: Option<usize>,
    /// Maximum number of rows (array/blocks). Validated on create/update.
    #[serde(default)]
    pub max_rows: Option<usize>,
    /// Minimum string length (text/textarea). Validated server-side.
    #[serde(default)]
    pub min_length: Option<usize>,
    /// Maximum string length (text/textarea). Validated server-side + HTML attr.
    #[serde(default)]
    pub max_length: Option<usize>,
    /// Minimum numeric value (number fields). Validated server-side + HTML attr.
    #[serde(default)]
    pub min: Option<f64>,
    /// Maximum numeric value (number fields). Validated server-side + HTML attr.
    #[serde(default)]
    pub max: Option<f64>,
    /// Allow multiple values (select). Stored as JSON array in TEXT column.
    #[serde(default)]
    pub has_many: bool,
    /// Minimum date value (date fields). ISO format: "2024-01-01". Validated server-side + HTML min attr.
    #[serde(default)]
    pub min_date: Option<String>,
    /// Maximum date value (date fields). ISO format: "2025-12-31". Validated server-side + HTML max attr.
    #[serde(default)]
    pub max_date: Option<String>,
    /// Configuration for join (virtual reverse-relationship) fields.
    #[serde(default)]
    pub join: Option<JoinConfig>,
}

impl Default for FieldDefinition {
    fn default() -> Self {
        Self {
            name: String::new(),
            field_type: FieldType::Text,
            required: false,
            unique: false,
            index: false,
            validate: None,
            default_value: None,
            options: Vec::new(),
            admin: FieldAdmin::default(),
            hooks: FieldHooks::default(),
            access: FieldAccess::default(),
            mcp: McpFieldConfig::default(),
            relationship: None,
            fields: Vec::new(),
            blocks: Vec::new(),
            tabs: Vec::new(),
            localized: false,
            picker_appearance: None,
            min_rows: None,
            max_rows: None,
            min_length: None,
            max_length: None,
            min: None,
            max: None,
            has_many: false,
            min_date: None,
            max_date: None,
            join: None,
        }
    }
}

/// Recursively flatten layout wrappers (Row, Collapsible, Tabs) to extract leaf fields.
/// Used by Array join table DDL, read, write, and form parsing — layout wrappers are
/// transparent inside arrays, so their children should be promoted as individual columns.
pub fn flatten_array_sub_fields(fields: &[FieldDefinition]) -> Vec<&FieldDefinition> {
    let mut result = Vec::new();
    for f in fields {
        match f.field_type {
            FieldType::Row | FieldType::Collapsible => {
                result.extend(flatten_array_sub_fields(&f.fields));
            }
            FieldType::Tabs => {
                for tab in &f.tabs {
                    result.extend(flatten_array_sub_fields(&tab.fields));
                }
            }
            _ => result.push(f),
        }
    }
    result
}

impl FieldDefinition {
    /// Whether this field has a column on the parent table.
    /// False for Array, Group, Row, Blocks, and has-many Relationship (they use join tables or prefixed/promoted columns).
    pub fn has_parent_column(&self) -> bool {
        match self.field_type {
            FieldType::Array => false,
            FieldType::Group => false, // sub-fields get prefixed columns instead
            FieldType::Row => false,         // sub-fields promoted to parent level (no prefix)
            FieldType::Collapsible => false, // sub-fields promoted to parent level (no prefix)
            FieldType::Tabs => false,        // sub-fields promoted to parent level (no prefix)
            FieldType::Blocks => false,      // uses a join table
            FieldType::Join => false,        // virtual field, no column
            FieldType::Relationship | FieldType::Upload => {
                match &self.relationship {
                    Some(rc) => !rc.has_many,
                    None => true, // default to has-one
                }
            }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_known_types() {
        assert_eq!(FieldType::from_str("text"), FieldType::Text);
        assert_eq!(FieldType::from_str("number"), FieldType::Number);
        assert_eq!(FieldType::from_str("textarea"), FieldType::Textarea);
        assert_eq!(FieldType::from_str("select"), FieldType::Select);
        assert_eq!(FieldType::from_str("radio"), FieldType::Radio);
        assert_eq!(FieldType::from_str("checkbox"), FieldType::Checkbox);
        assert_eq!(FieldType::from_str("date"), FieldType::Date);
        assert_eq!(FieldType::from_str("email"), FieldType::Email);
        assert_eq!(FieldType::from_str("json"), FieldType::Json);
        assert_eq!(FieldType::from_str("richtext"), FieldType::Richtext);
        assert_eq!(FieldType::from_str("relationship"), FieldType::Relationship);
        assert_eq!(FieldType::from_str("array"), FieldType::Array);
        assert_eq!(FieldType::from_str("group"), FieldType::Group);
        assert_eq!(FieldType::from_str("upload"), FieldType::Upload);
        assert_eq!(FieldType::from_str("blocks"), FieldType::Blocks);
        assert_eq!(FieldType::from_str("code"), FieldType::Code);
        assert_eq!(FieldType::from_str("join"), FieldType::Join);
    }

    #[test]
    fn from_str_case_insensitive() {
        assert_eq!(FieldType::from_str("TEXT"), FieldType::Text);
        assert_eq!(FieldType::from_str("Number"), FieldType::Number);
    }

    #[test]
    fn from_str_unknown_defaults_to_text() {
        assert_eq!(FieldType::from_str("unknown"), FieldType::Text);
        assert_eq!(FieldType::from_str(""), FieldType::Text);
    }

    #[test]
    fn sqlite_type_mapping() {
        assert_eq!(FieldType::Text.sqlite_type(), "TEXT");
        assert_eq!(FieldType::Number.sqlite_type(), "REAL");
        assert_eq!(FieldType::Checkbox.sqlite_type(), "INTEGER");
        assert_eq!(FieldType::Json.sqlite_type(), "TEXT");
        assert_eq!(FieldType::Richtext.sqlite_type(), "TEXT");
        assert_eq!(FieldType::Relationship.sqlite_type(), "TEXT");
    }

    #[test]
    fn as_str_roundtrip() {
        let types = [
            FieldType::Text, FieldType::Number, FieldType::Textarea,
            FieldType::Select, FieldType::Radio, FieldType::Checkbox, FieldType::Date,
            FieldType::Email, FieldType::Json, FieldType::Richtext,
            FieldType::Relationship, FieldType::Array,
            FieldType::Group, FieldType::Upload, FieldType::Blocks,
            FieldType::Row, FieldType::Collapsible, FieldType::Tabs,
            FieldType::Code, FieldType::Join,
        ];
        for ft in &types {
            assert_eq!(FieldType::from_str(ft.as_str()), *ft);
        }
    }

    // ── LocalizedString tests ───────────────────────────────────────────────

    #[test]
    fn localized_string_resolve_existing_locale() {
        let mut map = HashMap::new();
        map.insert("en".to_string(), "Title".to_string());
        map.insert("de".to_string(), "Titel".to_string());
        let ls = LocalizedString::Localized(map);
        assert_eq!(ls.resolve("de", "en"), "Titel");
    }

    #[test]
    fn localized_string_resolve_fallback_to_default() {
        let mut map = HashMap::new();
        map.insert("en".to_string(), "Title".to_string());
        let ls = LocalizedString::Localized(map);
        // Requesting "fr" which doesn't exist, should fall back to "en"
        assert_eq!(ls.resolve("fr", "en"), "Title");
    }

    #[test]
    fn localized_string_resolve_default_plain() {
        let ls = LocalizedString::Plain("Hello".to_string());
        assert_eq!(ls.resolve_default(), "Hello");
        assert_eq!(ls.resolve("de", "en"), "Hello");
    }

    #[test]
    fn localized_string_resolve_default_empty() {
        let map = HashMap::new();
        let ls = LocalizedString::Localized(map);
        assert_eq!(ls.resolve("en", "en"), "");
        assert_eq!(ls.resolve_default(), "");
    }

    // ── has_parent_column tests ─────────────────────────────────────────────

    #[test]
    fn has_parent_column_scalar_types() {
        for ft in [FieldType::Text, FieldType::Number, FieldType::Textarea,
                    FieldType::Select, FieldType::Checkbox, FieldType::Date,
                    FieldType::Email, FieldType::Json, FieldType::Richtext,
                    FieldType::Upload] {
            let f = FieldDefinition { field_type: ft.clone(), ..Default::default() };
            assert!(f.has_parent_column(), "{:?} should have parent column", ft);
        }
    }

    #[test]
    fn has_parent_column_array_false() {
        let f = FieldDefinition { field_type: FieldType::Array, ..Default::default() };
        assert!(!f.has_parent_column());
    }

    #[test]
    fn has_parent_column_group_false() {
        let f = FieldDefinition { field_type: FieldType::Group, ..Default::default() };
        assert!(!f.has_parent_column());
    }

    #[test]
    fn has_parent_column_blocks_false() {
        let f = FieldDefinition { field_type: FieldType::Blocks, ..Default::default() };
        assert!(!f.has_parent_column());
    }

    #[test]
    fn has_parent_column_row_false() {
        let f = FieldDefinition { field_type: FieldType::Row, ..Default::default() };
        assert!(!f.has_parent_column(), "Row should not have parent column");
    }

    #[test]
    fn row_from_str() {
        assert_eq!(FieldType::from_str("row"), FieldType::Row);
        assert_eq!(FieldType::Row.as_str(), "row");
    }

    #[test]
    fn collapsible_from_str() {
        assert_eq!(FieldType::from_str("collapsible"), FieldType::Collapsible);
        assert_eq!(FieldType::Collapsible.as_str(), "collapsible");
    }

    #[test]
    fn tabs_from_str() {
        assert_eq!(FieldType::from_str("tabs"), FieldType::Tabs);
        assert_eq!(FieldType::Tabs.as_str(), "tabs");
    }

    #[test]
    fn has_parent_column_collapsible_false() {
        let f = FieldDefinition { field_type: FieldType::Collapsible, ..Default::default() };
        assert!(!f.has_parent_column(), "Collapsible should not have parent column");
    }

    #[test]
    fn has_parent_column_tabs_false() {
        let f = FieldDefinition { field_type: FieldType::Tabs, ..Default::default() };
        assert!(!f.has_parent_column(), "Tabs should not have parent column");
    }

    #[test]
    fn has_parent_column_relationship_has_one() {
        let f = FieldDefinition {
            field_type: FieldType::Relationship,
            relationship: Some(RelationshipConfig {
                collection: "posts".to_string(),
                has_many: false,
                max_depth: None,
                polymorphic: vec![],
            }),
            ..Default::default()
        };
        assert!(f.has_parent_column(), "has-one relationship should have parent column");
    }

    #[test]
    fn has_parent_column_relationship_has_many() {
        let f = FieldDefinition {
            field_type: FieldType::Relationship,
            relationship: Some(RelationshipConfig {
                collection: "tags".to_string(),
                has_many: true,
                max_depth: None,
                polymorphic: vec![],
            }),
            ..Default::default()
        };
        assert!(!f.has_parent_column(), "has-many relationship should not have parent column");
    }

    #[test]
    fn has_parent_column_relationship_no_config() {
        let f = FieldDefinition {
            field_type: FieldType::Relationship,
            relationship: None,
            ..Default::default()
        };
        assert!(f.has_parent_column(), "relationship with no config defaults to has-one");
    }

    #[test]
    fn has_parent_column_upload_has_many_false() {
        let f = FieldDefinition {
            field_type: FieldType::Upload,
            relationship: Some(RelationshipConfig {
                collection: "media".to_string(),
                has_many: true,
                max_depth: None,
                polymorphic: vec![],
            }),
            ..Default::default()
        };
        assert!(!f.has_parent_column(), "has-many upload should not have parent column");
    }

    #[test]
    fn has_parent_column_upload_has_one_true() {
        let f = FieldDefinition {
            field_type: FieldType::Upload,
            relationship: Some(RelationshipConfig {
                collection: "media".to_string(),
                has_many: false,
                max_depth: None,
                polymorphic: vec![],
            }),
            ..Default::default()
        };
        assert!(f.has_parent_column(), "has-one upload should have parent column");
    }

    // ── FieldHooks tests ──────────────────────────────────────────────────

    #[test]
    fn field_hooks_is_empty_default() {
        let hooks = FieldHooks::default();
        assert!(hooks.is_empty());
    }

    #[test]
    fn field_hooks_is_empty_with_hooks() {
        let hooks = FieldHooks {
            before_validate: vec!["validate_slug".to_string()],
            ..Default::default()
        };
        assert!(!hooks.is_empty());
    }

    // ── FieldDefinition default tests ─────────────────────────────────────

    #[test]
    fn field_definition_default() {
        let f = FieldDefinition::default();
        assert_eq!(f.name, "");
        assert_eq!(f.field_type, FieldType::Text);
        assert!(!f.required);
        assert!(!f.unique);
        assert!(!f.index);
        assert!(!f.localized);
        assert!(f.validate.is_none());
        assert!(f.default_value.is_none());
        assert!(f.options.is_empty());
        assert!(f.relationship.is_none());
        assert!(f.fields.is_empty());
        assert!(f.blocks.is_empty());
        assert!(f.tabs.is_empty());
        assert!(f.min_rows.is_none());
        assert!(f.max_rows.is_none());
        assert!(f.min_length.is_none());
        assert!(f.max_length.is_none());
        assert!(f.min.is_none());
        assert!(f.max.is_none());
    }

    // ── flatten_array_sub_fields tests ────────────────────────────────────

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Text,
            ..Default::default()
        }
    }

    #[test]
    fn flatten_array_sub_fields_basic() {
        let fields = vec![
            text_field("title"),
            FieldDefinition {
                name: "layout".to_string(),
                field_type: FieldType::Row,
                fields: vec![text_field("slug"), text_field("author")],
                ..Default::default()
            },
            FieldDefinition {
                name: "settings".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![
                    FieldTab {
                        label: "General".to_string(),
                        description: None,
                        fields: vec![text_field("color")],
                    },
                    FieldTab {
                        label: "Advanced".to_string(),
                        description: None,
                        fields: vec![text_field("cache")],
                    },
                ],
                ..Default::default()
            },
        ];
        let flat = flatten_array_sub_fields(&fields);
        let names: Vec<&str> = flat.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["title", "slug", "author", "color", "cache"]);
    }

    #[test]
    fn flatten_array_sub_fields_nested() {
        // Row inside Tabs
        let fields = vec![
            FieldDefinition {
                name: "layout".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![FieldTab {
                    label: "Tab".to_string(),
                    description: None,
                    fields: vec![
                        FieldDefinition {
                            name: "row".to_string(),
                            field_type: FieldType::Row,
                            fields: vec![text_field("a"), text_field("b")],
                            ..Default::default()
                        },
                    ],
                }],
                ..Default::default()
            },
        ];
        let flat = flatten_array_sub_fields(&fields);
        let names: Vec<&str> = flat.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn flatten_array_sub_fields_empty() {
        let flat = flatten_array_sub_fields(&[]);
        assert!(flat.is_empty());
    }

    #[test]
    fn flatten_array_sub_fields_collapsible() {
        let fields = vec![
            FieldDefinition {
                name: "advanced".to_string(),
                field_type: FieldType::Collapsible,
                fields: vec![text_field("x"), text_field("y")],
                ..Default::default()
            },
        ];
        let flat = flatten_array_sub_fields(&fields);
        let names: Vec<&str> = flat.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["x", "y"]);
    }

    // ── richtext_format tests ─────────────────────────────────────────────

    #[test]
    fn richtext_format_default_is_none() {
        let admin = FieldAdmin::default();
        assert!(admin.richtext_format.is_none());
    }
}
