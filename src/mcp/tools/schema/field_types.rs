//! Field-type metadata exposed via the `list_field_types` MCP tool.

use anyhow::Result;
use serde::Serialize;
use serde_json::to_string_pretty;

/// Single entry in the `list_field_types` MCP tool response.
#[derive(Serialize)]
struct FieldTypeInfo {
    name: &'static str,
    description: &'static str,
    json_schema_type: &'static str,
    supports_has_many: bool,
    supports_sub_fields: bool,
    supports_options: bool,
}

const FIELD_TYPES: &[FieldTypeInfo] = &[
    FieldTypeInfo {
        name: "text",
        description: "Single-line text input",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "number",
        description: "Numeric input (integer or float)",
        json_schema_type: "number",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "textarea",
        description: "Multi-line text input",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "select",
        description: "Dropdown select from predefined options",
        json_schema_type: "string",
        supports_has_many: true,
        supports_sub_fields: false,
        supports_options: true,
    },
    FieldTypeInfo {
        name: "radio",
        description: "Radio button group from predefined options",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: true,
    },
    FieldTypeInfo {
        name: "checkbox",
        description: "Boolean checkbox (true/false)",
        json_schema_type: "boolean",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "date",
        description: "Date/datetime picker",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "email",
        description: "Email address input with validation",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "json",
        description: "Raw JSON data stored as text",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "richtext",
        description: "Rich text editor (HTML content)",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "code",
        description: "Code editor with syntax highlighting",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "relationship",
        description: "Reference to document(s) in another collection",
        json_schema_type: "string",
        supports_has_many: true,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "array",
        description: "Repeatable group of sub-fields (stored in join table)",
        json_schema_type: "array",
        supports_has_many: false,
        supports_sub_fields: true,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "group",
        description: "Named group of sub-fields (columns prefixed with group name)",
        json_schema_type: "object",
        supports_has_many: false,
        supports_sub_fields: true,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "upload",
        description: "File upload field referencing an upload collection",
        json_schema_type: "string",
        supports_has_many: true,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "blocks",
        description: "Flexible content blocks with different block types",
        json_schema_type: "array",
        supports_has_many: false,
        supports_sub_fields: true,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "row",
        description: "Layout-only horizontal container. Sub-fields promoted to parent level (no prefix)",
        json_schema_type: "null",
        supports_has_many: false,
        supports_sub_fields: true,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "collapsible",
        description: "Layout-only collapsible container. Sub-fields promoted to parent level (no prefix)",
        json_schema_type: "null",
        supports_has_many: false,
        supports_sub_fields: true,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "tabs",
        description: "Layout-only tabbed container. Sub-fields promoted to parent level (no prefix)",
        json_schema_type: "null",
        supports_has_many: false,
        supports_sub_fields: true,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "join",
        description: "Virtual reverse-relationship field. Shows documents from another collection that reference this document. No stored data.",
        json_schema_type: "null",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
];

/// List all available field types with their capabilities.
pub(in crate::mcp::tools) fn exec_list_field_types() -> Result<String> {
    Ok(to_string_pretty(FIELD_TYPES)?)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, from_str};

    use super::*;

    #[test]
    fn list_field_types_returns_all_types() {
        let result = exec_list_field_types().unwrap();
        let types: Vec<Value> = from_str(&result).unwrap();
        assert_eq!(types.len(), 20);

        // Verify all expected field types are present
        let names: Vec<&str> = types.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for expected in &[
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
        ] {
            assert!(names.contains(expected), "Missing field type: {expected}");
        }

        // Verify each entry has all required keys
        for t in &types {
            assert!(t.get("name").is_some());
            assert!(t.get("description").is_some());
            assert!(t.get("json_schema_type").is_some());
            assert!(t.get("supports_has_many").is_some());
            assert!(t.get("supports_sub_fields").is_some());
            assert!(t.get("supports_options").is_some());
        }
    }
}
