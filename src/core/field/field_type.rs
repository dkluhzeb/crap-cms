//! Supported field types. Each variant maps to a database column type (or join table).

use serde::{Deserialize, Serialize};

use crate::typegen::lua::LuaAlias;

/// Supported field types for collection and global definitions.
//
// Variant order mirrors the Lua-side autocomplete UX (scalars first,
// then choice types, then date/email/json/code, then relationships,
// then composites, then layout-only). The `LuaAlias` derive forwards
// the `///` doc-comment above into the user-facing Lua type file —
// keep it user-focused, no implementation chatter.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, LuaAlias)]
#[serde(rename_all = "lowercase")]
#[lua(alias = "crap.FieldType", rename_all = "lowercase")]
pub enum FieldType {
    /// Single-line string
    #[default]
    #[lua(view_class = "crap.TextField")]
    Text,
    /// Integer or float
    #[lua(view_class = "crap.NumberField")]
    Number,
    /// Multi-line text
    #[lua(view_class = "crap.TextareaField")]
    Textarea,
    /// Rich text (stored as HTML by default, or JSON with admin.format = "json")
    #[lua(view_class = "crap.RichtextField")]
    Richtext,
    /// Single or multi select from options (`has_many` for multi)
    #[lua(view_class = "crap.SelectField")]
    Select,
    /// Radio button group (same as select, renders as radio buttons)
    #[lua(view_class = "crap.RadioField")]
    Radio,
    /// Boolean (true/false)
    #[lua(view_class = "crap.CheckboxField")]
    Checkbox,
    /// ISO 8601 date/datetime
    #[lua(view_class = "crap.DateField")]
    Date,
    /// Validated email address
    #[lua(view_class = "crap.EmailField")]
    Email,
    /// Arbitrary JSON blob
    #[lua(view_class = "crap.JsonField")]
    Json,
    /// File upload (references media collection; `has_many` for multi-file)
    #[lua(view_class = "crap.UploadField")]
    Upload,
    /// Reference to another collection
    #[lua(view_class = "crap.RelationshipField")]
    Relationship,
    /// Repeatable sub-fields
    #[lua(view_class = "crap.ArrayField")]
    Array,
    /// Visual grouping (no extra table)
    #[lua(view_class = "crap.GroupField")]
    Group,
    /// Flexible content blocks
    #[lua(view_class = "crap.BlocksField")]
    Blocks,
    /// Layout-only horizontal grouping (no prefix)
    #[lua(view_class = "crap.RowField")]
    Row,
    /// Layout-only collapsible section (no prefix)
    #[lua(view_class = "crap.CollapsibleField")]
    Collapsible,
    /// Layout-only tabbed container (no prefix)
    #[lua(view_class = "crap.TabsField")]
    Tabs,
    /// Code editor (`CodeMirror`, `admin.language` for mode)
    #[lua(view_class = "crap.CodeField")]
    Code,
    /// Virtual reverse relationship (read-only, no column)
    //
    // The `crap.JoinField` view is populated by `FieldDefinition.join`'s
    // `#[lua(applies_to = "join", flatten)]` — `JoinConfig`'s fields
    // are inlined directly onto the subclass since the Lua surface
    // flattens them to the top level.
    #[lua(view_class = "crap.JoinField")]
    Join,
}

impl FieldType {
    /// True for the transparent layout wrappers (Row/Collapsible/Tabs) — they
    /// contribute nothing to column/table names and every walk treats their
    /// sub-fields as living at the wrapper's own level.
    #[must_use]
    pub fn is_layout_wrapper(&self) -> bool {
        matches!(
            self,
            FieldType::Row | FieldType::Collapsible | FieldType::Tabs
        )
    }

    /// The complete set of valid field-type strings — the frozen public
    /// contract. Used for strict parsing and for did-you-mean error messages.
    pub const ALL: &'static [&'static str] = &[
        "text",
        "number",
        "textarea",
        "select",
        "radio",
        "checkbox",
        "date",
        "email",
        "json",
        "richtext",
        "relationship",
        "array",
        "group",
        "upload",
        "blocks",
        "row",
        "collapsible",
        "tabs",
        "code",
        "join",
    ];

    /// Strictly parse a string into a `FieldType`. Returns `None` for any
    /// unrecognized type — callers MUST surface an error rather than
    /// defaulting, so that a typo'd or future type name is never silently
    /// stored as a `Text` column (which would freeze that column's shape).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_lowercase().as_str() {
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
            _ => return None,
        })
    }

    /// Parse a string into a `FieldType`, defaulting to `Text` if unknown.
    /// Prefer [`FieldType::parse`] at user-input boundaries — this exists only
    /// for internal callers where an unknown value genuinely cannot occur.
    #[must_use]
    pub fn parse_lossy(s: &str) -> Self {
        Self::parse(s).unwrap_or_else(|| {
            tracing::warn!("Unknown field type '{}', defaulting to Text", s);
            FieldType::Text
        })
    }

    /// Whether this field type is allowed as a richtext node attribute.
    ///
    /// Only scalar types that can be rendered as a simple form input in the
    /// node edit modal are allowed. Complex/structural types are rejected at
    /// registration time.
    #[must_use]
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
    #[must_use]
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
                "{ft:?} should be a valid node attr type"
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
                "{ft:?} should NOT be a valid node attr type"
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
