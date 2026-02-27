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
}

impl FieldType {
    pub fn sqlite_type(&self) -> &'static str {
        match self {
            FieldType::Text => "TEXT",
            FieldType::Number => "REAL",
            FieldType::Textarea => "TEXT",
            FieldType::Select => "TEXT",
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
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "text" => FieldType::Text,
            "number" => FieldType::Number,
            "textarea" => FieldType::Textarea,
            "select" => FieldType::Select,
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
        }
    }
}

/// A label/value pair for select field options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectOption {
    pub label: LocalizedString,
    pub value: String,
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
}

/// Admin UI display hints for a field (placeholder, description, visibility, width).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    /// For group fields: start collapsed in the admin UI.
    #[serde(default)]
    pub collapsed: bool,
    /// Sub-field name to use as row label (arrays/blocks).
    #[serde(default)]
    pub label_field: Option<String>,
    /// Lua function ref for computed row labels (arrays/blocks).
    #[serde(default)]
    pub row_label: Option<String>,
    /// For array/blocks: render rows collapsed by default.
    #[serde(default)]
    pub init_collapsed: bool,
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
    pub relationship: Option<RelationshipConfig>,
    #[serde(default)]
    pub fields: Vec<FieldDefinition>,
    #[serde(default)]
    pub blocks: Vec<BlockDefinition>,
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
}

impl Default for FieldDefinition {
    fn default() -> Self {
        Self {
            name: String::new(),
            field_type: FieldType::Text,
            required: false,
            unique: false,
            validate: None,
            default_value: None,
            options: Vec::new(),
            admin: FieldAdmin::default(),
            hooks: FieldHooks::default(),
            access: FieldAccess::default(),
            relationship: None,
            fields: Vec::new(),
            blocks: Vec::new(),
            localized: false,
            picker_appearance: None,
            min_rows: None,
            max_rows: None,
        }
    }
}

impl FieldDefinition {
    /// Whether this field has a column on the parent table.
    /// False for Array, Group, Blocks, and has-many Relationship (they use join tables or prefixed columns).
    pub fn has_parent_column(&self) -> bool {
        match self.field_type {
            FieldType::Array => false,
            FieldType::Group => false, // sub-fields get prefixed columns instead
            FieldType::Blocks => false, // uses a join table
            FieldType::Relationship => {
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
            FieldType::Select, FieldType::Checkbox, FieldType::Date,
            FieldType::Email, FieldType::Json, FieldType::Richtext,
            FieldType::Relationship, FieldType::Array,
            FieldType::Group, FieldType::Upload, FieldType::Blocks,
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
    fn has_parent_column_relationship_has_one() {
        let f = FieldDefinition {
            field_type: FieldType::Relationship,
            relationship: Some(RelationshipConfig {
                collection: "posts".to_string(),
                has_many: false,
                max_depth: None,
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
        assert!(!f.localized);
        assert!(f.validate.is_none());
        assert!(f.default_value.is_none());
        assert!(f.options.is_empty());
        assert!(f.relationship.is_none());
        assert!(f.fields.is_empty());
        assert!(f.blocks.is_empty());
        assert!(f.min_rows.is_none());
        assert!(f.max_rows.is_none());
    }
}
