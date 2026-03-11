//! Supported field types. Each variant maps to a SQLite column type (or join table).

use serde::{Deserialize, Serialize};

/// Supported field types. Each variant maps to a SQLite column type (or join table for Array/Blocks/has-many).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    /// Single-line plain text.
    Text,
    /// Floating-point or integer number.
    Number,
    /// Multi-line plain text area.
    Textarea,
    /// Dropdown selection from predefined options.
    Select,
    /// Single checkbox for boolean values.
    Checkbox,
    /// Date or date-time string.
    Date,
    /// Email address with basic validation.
    Email,
    /// Structured JSON data stored as text.
    Json,
    /// Rich text content (e.g. HTML from a WYSIWYG editor).
    Richtext,
    /// Relationship to another document (many-to-one).
    Relationship,
    /// Array of sub-fields (many-to-many relationship).
    Array,
    /// Group of sub-fields (prefixed columns in the same table).
    Group,
    /// Reference to a file in an upload collection.
    Upload,
    /// Dynamic blocks of different field sets.
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

impl FieldType {
    /// Returns the SQLite column type for this field type.
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
            FieldType::Row => "TEXT",    // never used — sub-fields are promoted to parent
            FieldType::Collapsible => "TEXT", // never used — sub-fields are promoted to parent
            FieldType::Tabs => "TEXT",   // never used — sub-fields are promoted to parent
            FieldType::Code => "TEXT",
            FieldType::Join => "TEXT", // never used — virtual field, no column
        }
    }

    /// Parse a string into a `FieldType`, defaulting to `Text` if unknown.
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

    /// Returns the string identifier for this field type.
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
            FieldType::Text,
            FieldType::Number,
            FieldType::Textarea,
            FieldType::Select,
            FieldType::Radio,
            FieldType::Checkbox,
            FieldType::Date,
            FieldType::Email,
            FieldType::Json,
            FieldType::Richtext,
            FieldType::Relationship,
            FieldType::Array,
            FieldType::Group,
            FieldType::Upload,
            FieldType::Blocks,
            FieldType::Row,
            FieldType::Collapsible,
            FieldType::Tabs,
            FieldType::Code,
            FieldType::Join,
        ];
        for ft in &types {
            assert_eq!(FieldType::from_str(ft.as_str()), *ft);
        }
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
}
