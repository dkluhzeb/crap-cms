//! Supported field types. Each variant maps to a database column type (or join table).

use serde::{Deserialize, Serialize};

/// Supported field types. Each variant maps to a database column type (or join table for Array/Blocks/has-many).
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
    /// Parse a string into a `FieldType`, defaulting to `Text` if unknown.
    pub fn parse_lossy(s: &str) -> Self {
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

    /// Whether this field type is allowed as a richtext node attribute.
    ///
    /// Only scalar types that can be rendered as a simple form input in the
    /// node edit modal are allowed. Complex/structural types are rejected at
    /// registration time.
    pub fn is_node_attr_type(&self) -> bool {
        matches!(
            self,
            FieldType::Text
                | FieldType::Number
                | FieldType::Textarea
                | FieldType::Select
                | FieldType::Radio
                | FieldType::Checkbox
                | FieldType::Date
                | FieldType::Email
                | FieldType::Json
                | FieldType::Code
        )
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
        assert_eq!(FieldType::parse_lossy("text"), FieldType::Text);
        assert_eq!(FieldType::parse_lossy("number"), FieldType::Number);
        assert_eq!(FieldType::parse_lossy("textarea"), FieldType::Textarea);
        assert_eq!(FieldType::parse_lossy("select"), FieldType::Select);
        assert_eq!(FieldType::parse_lossy("radio"), FieldType::Radio);
        assert_eq!(FieldType::parse_lossy("checkbox"), FieldType::Checkbox);
        assert_eq!(FieldType::parse_lossy("date"), FieldType::Date);
        assert_eq!(FieldType::parse_lossy("email"), FieldType::Email);
        assert_eq!(FieldType::parse_lossy("json"), FieldType::Json);
        assert_eq!(FieldType::parse_lossy("richtext"), FieldType::Richtext);
        assert_eq!(
            FieldType::parse_lossy("relationship"),
            FieldType::Relationship
        );
        assert_eq!(FieldType::parse_lossy("array"), FieldType::Array);
        assert_eq!(FieldType::parse_lossy("group"), FieldType::Group);
        assert_eq!(FieldType::parse_lossy("upload"), FieldType::Upload);
        assert_eq!(FieldType::parse_lossy("blocks"), FieldType::Blocks);
        assert_eq!(FieldType::parse_lossy("code"), FieldType::Code);
        assert_eq!(FieldType::parse_lossy("join"), FieldType::Join);
    }

    #[test]
    fn from_str_case_insensitive() {
        assert_eq!(FieldType::parse_lossy("TEXT"), FieldType::Text);
        assert_eq!(FieldType::parse_lossy("Number"), FieldType::Number);
    }

    #[test]
    fn from_str_unknown_defaults_to_text() {
        assert_eq!(FieldType::parse_lossy("unknown"), FieldType::Text);
        assert_eq!(FieldType::parse_lossy(""), FieldType::Text);
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
            assert_eq!(FieldType::parse_lossy(ft.as_str()), *ft);
        }
    }

    #[test]
    fn is_node_attr_type_allowed() {
        let allowed = [
            FieldType::Text,
            FieldType::Number,
            FieldType::Textarea,
            FieldType::Select,
            FieldType::Radio,
            FieldType::Checkbox,
            FieldType::Date,
            FieldType::Email,
            FieldType::Json,
            FieldType::Code,
        ];
        for ft in &allowed {
            assert!(
                ft.is_node_attr_type(),
                "{:?} should be a valid node attr type",
                ft
            );
        }
    }

    #[test]
    fn is_node_attr_type_rejected() {
        let rejected = [
            FieldType::Relationship,
            FieldType::Upload,
            FieldType::Array,
            FieldType::Group,
            FieldType::Row,
            FieldType::Collapsible,
            FieldType::Tabs,
            FieldType::Blocks,
            FieldType::Join,
            FieldType::Richtext,
        ];
        for ft in &rejected {
            assert!(
                !ft.is_node_attr_type(),
                "{:?} should NOT be a valid node attr type",
                ft
            );
        }
    }

    #[test]
    fn row_from_str() {
        assert_eq!(FieldType::parse_lossy("row"), FieldType::Row);
        assert_eq!(FieldType::Row.as_str(), "row");
    }

    #[test]
    fn collapsible_from_str() {
        assert_eq!(
            FieldType::parse_lossy("collapsible"),
            FieldType::Collapsible
        );
        assert_eq!(FieldType::Collapsible.as_str(), "collapsible");
    }

    #[test]
    fn tabs_from_str() {
        assert_eq!(FieldType::parse_lossy("tabs"), FieldType::Tabs);
        assert_eq!(FieldType::Tabs.as_str(), "tabs");
    }
}
