//! Field-type catalogues used by the parser, writer, and wizard.

/// Valid field types for collection definitions.
pub const VALID_FIELD_TYPES: &[&str] = &[
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
    "code",
    "relationship",
    "array",
    "group",
    "upload",
    "blocks",
    "row",
    "collapsible",
    "tabs",
    "join",
];

/// Container field types that support nested subfields.
pub const CONTAINER_TYPES: &[&str] = &["group", "array", "row", "collapsible"];
