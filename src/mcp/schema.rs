//! JSON Schema generation from `FieldDefinition` and `CollectionDefinition`.

use serde_json::{json, Value};

use crate::core::collection::{CollectionDefinition, GlobalDefinition};
use crate::core::field::{FieldDefinition, FieldType};

/// CRUD operation type, determines which fields are included/required in the schema.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CrudOp {
    Create,
    Update,
    Find,
    FindById,
    Delete,
}

/// Convert a single `FieldDefinition` to a JSON Schema value.
pub fn field_to_json_schema(field: &FieldDefinition) -> Value {
    let description = field.mcp.description.as_deref()
        .or(field.admin.description.as_ref().map(|ls| ls.resolve_default()));

    let mut schema = match field.field_type {
        FieldType::Text | FieldType::Textarea | FieldType::Email | FieldType::Code => {
            json!({ "type": "string" })
        }
        FieldType::Date => {
            json!({ "type": "string", "format": "date-time" })
        }
        FieldType::Number => {
            json!({ "type": "number" })
        }
        FieldType::Checkbox => {
            json!({ "type": "boolean" })
        }
        FieldType::Select | FieldType::Radio => {
            if field.options.is_empty() {
                json!({ "type": "string" })
            } else if field.has_many {
                let values: Vec<Value> = field.options.iter()
                    .map(|o| Value::String(o.value.clone()))
                    .collect();
                json!({
                    "type": "array",
                    "items": { "type": "string", "enum": values }
                })
            } else {
                let values: Vec<Value> = field.options.iter()
                    .map(|o| Value::String(o.value.clone()))
                    .collect();
                json!({ "type": "string", "enum": values })
            }
        }
        FieldType::Richtext => {
            json!({ "type": "string" })
        }
        FieldType::Json => {
            json!({})
        }
        FieldType::Relationship | FieldType::Upload => {
            let has_many = field.relationship.as_ref()
                .map(|r| r.has_many)
                .unwrap_or(field.has_many);
            if has_many {
                json!({ "type": "array", "items": { "type": "string" } })
            } else {
                json!({ "type": "string" })
            }
        }
        FieldType::Array => {
            let sub_schema = fields_to_object_schema(&field.fields);
            json!({ "type": "array", "items": sub_schema })
        }
        FieldType::Blocks => {
            if field.blocks.is_empty() {
                json!({ "type": "array" })
            } else {
                let variants: Vec<Value> = field.blocks.iter().map(|b| {
                    let mut props = serde_json::Map::new();
                    props.insert("blockType".to_string(), json!({ "type": "string", "const": b.block_type }));
                    for sf in &b.fields {
                        props.insert(sf.name.clone(), field_to_json_schema(sf));
                    }
                    json!({
                        "type": "object",
                        "properties": props,
                        "required": ["blockType"]
                    })
                }).collect();
                json!({
                    "type": "array",
                    "items": { "oneOf": variants }
                })
            }
        }
        FieldType::Group => {
            fields_to_object_schema(&field.fields)
        }
        // Layout-only types — sub-fields are promoted to parent level
        FieldType::Row | FieldType::Collapsible | FieldType::Tabs => {
            // These don't appear as individual JSON Schema properties;
            // their children are flattened. Return empty object as placeholder.
            json!({})
        }
        // Join fields are virtual/read-only — not included in input schemas
        FieldType::Join => {
            json!({ "type": "string" })
        }
    };

    if let Some(desc) = description {
        if let Some(obj) = schema.as_object_mut() {
            obj.insert("description".to_string(), Value::String(desc.to_string()));
        }
    }

    schema
}

/// Convert a list of `FieldDefinition`s to a JSON Schema `object` with `properties` and `required`.
fn fields_to_object_schema(fields: &[FieldDefinition]) -> Value {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();

    for field in fields {
        match field.field_type {
            // Layout types: promote children to parent
            FieldType::Row | FieldType::Collapsible => {
                for sf in &field.fields {
                    props.insert(sf.name.clone(), field_to_json_schema(sf));
                    if sf.required {
                        required.push(Value::String(sf.name.clone()));
                    }
                }
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    for sf in &tab.fields {
                        props.insert(sf.name.clone(), field_to_json_schema(sf));
                        if sf.required {
                            required.push(Value::String(sf.name.clone()));
                        }
                    }
                }
            }
            // Join fields are read-only, skip
            FieldType::Join => {}
            _ => {
                props.insert(field.name.clone(), field_to_json_schema(field));
                if field.required {
                    required.push(Value::String(field.name.clone()));
                }
            }
        }
    }

    let mut schema = json!({
        "type": "object",
        "properties": props,
    });
    if !required.is_empty() {
        schema.as_object_mut().expect("json!({}) is Object")
            .insert("required".to_string(), Value::Array(required));
    }
    schema
}

/// Generate the input schema for a collection CRUD tool.
pub fn collection_input_schema(def: &CollectionDefinition, op: CrudOp) -> Value {
    match op {
        CrudOp::Create => {
            let mut schema = fields_to_object_schema(&def.fields);
            // Auth collections get a password field
            if def.is_auth_collection() {
                if let Some(obj) = schema.as_object_mut() {
                    if let Some(props) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
                        props.insert("password".to_string(), json!({ "type": "string" }));
                    }
                    if let Some(req) = obj.get_mut("required").and_then(|r| r.as_array_mut()) {
                        req.push(Value::String("password".to_string()));
                    }
                }
            }
            schema
        }
        CrudOp::Update => {
            let mut schema = fields_to_object_schema(&def.fields);
            if let Some(obj) = schema.as_object_mut() {
                // Add required id field
                if let Some(props) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
                    props.insert("id".to_string(), json!({ "type": "string" }));
                }
                obj.insert("required".to_string(), json!(["id"]));
                // Auth collections can update password
                if def.is_auth_collection() {
                    if let Some(props) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
                        props.insert("password".to_string(), json!({
                            "type": "string",
                            "description": "Leave empty to keep current password"
                        }));
                    }
                }
            }
            schema
        }
        CrudOp::Delete => {
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" }
                },
                "required": ["id"]
            })
        }
        CrudOp::FindById => {
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "depth": { "type": "integer", "description": "Relationship population depth" }
                },
                "required": ["id"]
            })
        }
        CrudOp::Find => {
            json!({
                "type": "object",
                "properties": {
                    "where": {
                        "type": "object",
                        "description": "Filter conditions. Keys are field names, values are filter objects (e.g. {\"equals\": \"value\"}, {\"contains\": \"text\"}, {\"greater_than\": 5})"
                    },
                    "order_by": { "type": "string", "description": "Sort field (prefix with - for descending)" },
                    "limit": { "type": "integer" },
                    "offset": { "type": "integer" },
                    "depth": { "type": "integer", "description": "Relationship population depth" },
                    "search": { "type": "string", "description": "Full-text search query" }
                }
            })
        }
    }
}

/// Generate the input schema for a global CRUD tool.
pub fn global_input_schema(def: &GlobalDefinition, op: CrudOp) -> Value {
    match op {
        CrudOp::Find => {
            // Read global — no params needed
            json!({ "type": "object", "properties": {} })
        }
        CrudOp::Update => {
            fields_to_object_schema(&def.fields)
        }
        _ => json!({ "type": "object", "properties": {} }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{SelectOption, McpFieldConfig, FieldAdmin};
    use crate::core::field::LocalizedString;

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn required_text(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            required: true,
            ..Default::default()
        }
    }

    #[test]
    fn text_field_schema() {
        let f = text_field("title");
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "string");
    }

    #[test]
    fn number_field_schema() {
        let f = FieldDefinition {
            name: "count".to_string(),
            field_type: FieldType::Number,
            ..Default::default()
        };
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "number");
    }

    #[test]
    fn checkbox_field_schema() {
        let f = FieldDefinition {
            name: "active".to_string(),
            field_type: FieldType::Checkbox,
            ..Default::default()
        };
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "boolean");
    }

    #[test]
    fn select_field_with_options() {
        let f = FieldDefinition {
            name: "status".to_string(),
            field_type: FieldType::Select,
            options: vec![
                SelectOption { label: LocalizedString::Plain("Draft".to_string()), value: "draft".to_string() },
                SelectOption { label: LocalizedString::Plain("Published".to_string()), value: "published".to_string() },
            ],
            ..Default::default()
        };
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "string");
        let enm = s["enum"].as_array().unwrap();
        assert_eq!(enm.len(), 2);
    }

    #[test]
    fn date_field_has_format() {
        let f = FieldDefinition {
            name: "created".to_string(),
            field_type: FieldType::Date,
            ..Default::default()
        };
        let s = field_to_json_schema(&f);
        assert_eq!(s["format"], "date-time");
    }

    #[test]
    fn relationship_has_many() {
        let f = FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Relationship,
            relationship: Some(crate::core::field::RelationshipConfig {
                collection: "tags".to_string(),
                has_many: true,
                max_depth: None,
                polymorphic: vec![],
            }),
            ..Default::default()
        };
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "array");
    }

    #[test]
    fn mcp_description_included() {
        let f = FieldDefinition {
            name: "status".to_string(),
            mcp: McpFieldConfig {
                description: Some("Publication status".to_string()),
            },
            ..Default::default()
        };
        let s = field_to_json_schema(&f);
        assert_eq!(s["description"], "Publication status");
    }

    #[test]
    fn admin_description_fallback() {
        let f = FieldDefinition {
            name: "status".to_string(),
            admin: FieldAdmin {
                description: Some(LocalizedString::Plain("Admin desc".to_string())),
                ..Default::default()
            },
            ..Default::default()
        };
        let s = field_to_json_schema(&f);
        assert_eq!(s["description"], "Admin desc");
    }

    #[test]
    fn collection_create_schema() {
        let def = CollectionDefinition {
            slug: "posts".to_string(),
            fields: vec![required_text("title"), text_field("body")],
            ..Default::default()
        };
        let s = collection_input_schema(&def, CrudOp::Create);
        assert!(s["properties"]["title"].is_object());
        assert!(s["properties"]["body"].is_object());
        let req = s["required"].as_array().unwrap();
        assert!(req.contains(&Value::String("title".to_string())));
    }

    #[test]
    fn collection_update_schema_has_id() {
        let def = CollectionDefinition {
            slug: "posts".to_string(),
            fields: vec![text_field("title")],
            ..Default::default()
        };
        let s = collection_input_schema(&def, CrudOp::Update);
        assert!(s["properties"]["id"].is_object());
        assert!(s["required"].as_array().unwrap().contains(&Value::String("id".to_string())));
    }

    #[test]
    fn collection_delete_schema() {
        let def = CollectionDefinition {
            slug: "posts".to_string(),
            fields: vec![],
            ..Default::default()
        };
        let s = collection_input_schema(&def, CrudOp::Delete);
        assert!(s["properties"]["id"].is_object());
    }

    #[test]
    fn collection_find_schema() {
        let def = CollectionDefinition {
            slug: "posts".to_string(),
            fields: vec![],
            ..Default::default()
        };
        let s = collection_input_schema(&def, CrudOp::Find);
        assert!(s["properties"]["where"].is_object());
        assert!(s["properties"]["limit"].is_object());
    }

    #[test]
    fn array_field_schema() {
        let f = FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            fields: vec![text_field("label"), required_text("value")],
            ..Default::default()
        };
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "array");
        assert!(s["items"]["properties"]["label"].is_object());
    }

    #[test]
    fn layout_fields_flattened() {
        let row = FieldDefinition {
            name: "row1".to_string(),
            field_type: FieldType::Row,
            fields: vec![text_field("first_name"), text_field("last_name")],
            ..Default::default()
        };
        let def = CollectionDefinition {
            slug: "people".to_string(),
            fields: vec![row],
            ..Default::default()
        };
        let s = collection_input_schema(&def, CrudOp::Create);
        // Row's children should be promoted
        assert!(s["properties"]["first_name"].is_object());
        assert!(s["properties"]["last_name"].is_object());
        // Row itself should not appear
        assert!(s["properties"]["row1"].is_null());
    }

    #[test]
    fn global_read_schema() {
        let def = GlobalDefinition {
            slug: "settings".to_string(),
            fields: vec![text_field("site_name")],
            ..Default::default()
        };
        let s = global_input_schema(&def, CrudOp::Find);
        assert!(s["properties"].is_object());
    }

    #[test]
    fn global_update_schema() {
        let def = GlobalDefinition {
            slug: "settings".to_string(),
            fields: vec![required_text("site_name")],
            ..Default::default()
        };
        let s = global_input_schema(&def, CrudOp::Update);
        assert!(s["properties"]["site_name"].is_object());
    }
}
