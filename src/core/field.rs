//! Field types and definitions. Each field maps to a column (or join table) in SQLite.

use serde::{Deserialize, Serialize};

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
    pub label: String,
    pub value: String,
}

/// A block type definition for Blocks fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockDefinition {
    pub block_type: String,
    pub fields: Vec<FieldDefinition>,
    #[serde(default)]
    pub label: Option<String>,
}

/// Admin UI display hints for a field (placeholder, description, visibility, width).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FieldAdmin {
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub readonly: bool,
    #[serde(default)]
    pub width: Option<String>,
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
}
