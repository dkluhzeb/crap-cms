//! JSON Schema generation from `FieldDefinition` and `CollectionDefinition`.

use serde_json::{Map, Value, json};

use crate::core::{
    collection::{CollectionDefinition, GlobalDefinition},
    field::{FieldDefinition, FieldType},
};

/// CRUD operation type, determines which fields are included/required in the schema.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CrudOp {
    Create,
    CreateMany,
    Update,
    UpdateMany,
    Find,
    FindById,
    Delete,
    DeleteMany,
    Undelete,
    Unpublish,
    Count,
    ListVersions,
    RestoreVersion,
}

/// Schema for Select/Radio fields, handling empty options, single, and has-many variants.
fn select_radio_schema(field: &FieldDefinition) -> Value {
    if field.options.is_empty() {
        return json!({ "type": "string" });
    }

    let values: Vec<Value> = field
        .options
        .iter()
        .map(|o| Value::String(o.value.clone()))
        .collect();

    if field.has_many {
        return json!({
            "type": "array",
            "items": { "type": "string", "enum": values }
        });
    }

    json!({ "type": "string", "enum": values })
}

/// Schema for Relationship/Upload fields — string or array of strings based on cardinality.
fn relationship_schema(field: &FieldDefinition) -> Value {
    let has_many = field
        .relationship
        .as_ref()
        .map(|r| r.has_many)
        .unwrap_or(field.has_many);

    if has_many {
        json!({ "type": "array", "items": { "type": "string" } })
    } else {
        json!({ "type": "string" })
    }
}

/// Schema for Blocks fields — array with `oneOf` variants per block type.
fn blocks_schema(field: &FieldDefinition) -> Value {
    if field.blocks.is_empty() {
        return json!({ "type": "array" });
    }

    let variants: Vec<Value> = field
        .blocks
        .iter()
        .map(|b| {
            let mut props = Map::new();
            props.insert(
                "blockType".to_string(),
                json!({ "type": "string", "const": b.block_type }),
            );

            for sf in &b.fields {
                props.insert(sf.name.clone(), field_to_json_schema(sf));
            }

            json!({
                "type": "object",
                "properties": props,
                "required": ["blockType"]
            })
        })
        .collect();

    json!({
        "type": "array",
        "items": { "oneOf": variants }
    })
}

/// Convert a single `FieldDefinition` to a JSON Schema value.
pub fn field_to_json_schema(field: &FieldDefinition) -> Value {
    let description = field.mcp.description.as_deref().or(field
        .admin
        .description
        .as_ref()
        .map(|ls| ls.resolve_default()));

    let mut schema = match field.field_type {
        FieldType::Text
        | FieldType::Textarea
        | FieldType::Email
        | FieldType::Code
        | FieldType::Richtext => json!({ "type": "string" }),
        FieldType::Date => json!({ "type": "string", "format": "date-time" }),
        FieldType::Number => json!({ "type": "number" }),
        FieldType::Checkbox => json!({ "type": "boolean" }),
        FieldType::Select | FieldType::Radio => select_radio_schema(field),
        FieldType::Json => json!({}),
        FieldType::Relationship | FieldType::Upload => relationship_schema(field),
        FieldType::Array => {
            json!({ "type": "array", "items": fields_to_object_schema(&field.fields) })
        }
        FieldType::Blocks => blocks_schema(field),
        FieldType::Group => fields_to_object_schema(&field.fields),
        FieldType::Row | FieldType::Collapsible | FieldType::Tabs => json!({}),
        FieldType::Join => json!({ "type": "string" }),
    };

    if let Some(desc) = description
        && let Some(obj) = schema.as_object_mut()
    {
        obj.insert("description".to_string(), Value::String(desc.to_string()));
    }

    schema
}

/// Insert a field into the schema properties, tracking required fields.
fn insert_prop(props: &mut Map<String, Value>, required: &mut Vec<Value>, field: &FieldDefinition) {
    props.insert(field.name.clone(), field_to_json_schema(field));

    if field.required {
        required.push(Value::String(field.name.clone()));
    }
}

/// Convert a list of `FieldDefinition`s to a JSON Schema `object` with `properties` and `required`.
fn fields_to_object_schema(fields: &[FieldDefinition]) -> Value {
    let mut props = Map::new();
    let mut required = Vec::new();

    for field in fields {
        match field.field_type {
            FieldType::Row | FieldType::Collapsible => {
                for sf in &field.fields {
                    insert_prop(&mut props, &mut required, sf);
                }
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    for sf in &tab.fields {
                        insert_prop(&mut props, &mut required, sf);
                    }
                }
            }
            FieldType::Join => {}
            _ => insert_prop(&mut props, &mut required, field),
        }
    }

    let mut schema = json!({
        "type": "object",
        "properties": props,
    });

    if !required.is_empty() {
        schema
            .as_object_mut()
            .expect("json!({}) is Object")
            .insert("required".to_string(), Value::Array(required));
    }

    schema
}

/// Helper: get the `properties` sub-object from a schema value.
fn get_props(schema: &mut Value) -> Option<&mut Map<String, Value>> {
    schema
        .as_object_mut()?
        .get_mut("properties")?
        .as_object_mut()
}

/// Input schema for collection create — includes required password for auth collections.
fn create_schema(def: &CollectionDefinition) -> Value {
    let mut schema = fields_to_object_schema(&def.fields);

    if !def.is_auth_collection() {
        return schema;
    }

    let Some(props) = get_props(&mut schema) else {
        return schema;
    };
    props.insert("password".to_string(), json!({ "type": "string" }));

    let obj = schema.as_object_mut().expect("schema is object");
    let req = obj
        .entry("required")
        .or_insert_with(|| Value::Array(Vec::new()));

    if let Some(arr) = req.as_array_mut() {
        arr.push(Value::String("password".to_string()));
    }

    schema
}

/// Input schema for collection update — requires id, optional password for auth collections.
fn update_schema(def: &CollectionDefinition) -> Value {
    let mut schema = fields_to_object_schema(&def.fields);

    let Some(props) = get_props(&mut schema) else {
        return schema;
    };
    props.insert("id".to_string(), json!({ "type": "string" }));

    if def.is_auth_collection() {
        props.insert(
            "password".to_string(),
            json!({
                "type": "string",
                "description": "Leave empty to keep current password"
            }),
        );
    }

    let obj = schema.as_object_mut().expect("schema is object");
    obj.insert("required".to_string(), json!(["id"]));

    schema
}

/// Schema requiring only an `id` field.
fn id_only_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "id": { "type": "string" } },
        "required": ["id"]
    })
}

/// Generate the input schema for a collection CRUD tool.
pub fn collection_input_schema(def: &CollectionDefinition, op: CrudOp) -> Value {
    match op {
        CrudOp::Create => create_schema(def),
        CrudOp::CreateMany => {
            let item_schema = create_schema(def);
            json!({
                "type": "object",
                "properties": {
                    "documents": {
                        "type": "array",
                        "items": item_schema,
                        "description": "Array of documents to create"
                    },
                    "hooks": {
                        "type": "boolean",
                        "description": "Run per-document lifecycle hooks (default: true)"
                    },
                    "draft": {
                        "type": "boolean",
                        "description": "Create documents as drafts (default: false)"
                    }
                },
                "required": ["documents"]
            })
        }
        CrudOp::Update => update_schema(def),
        CrudOp::UpdateMany => {
            let data_schema = fields_to_object_schema(&def.fields);
            json!({
                "type": "object",
                "properties": {
                    "where": {
                        "type": "object",
                        "description": "Filter conditions. Keys are field names, values are filter objects (e.g. {\"equals\": \"value\"}, {\"contains\": \"text\"})"
                    },
                    "data": {
                        "allOf": [data_schema],
                        "description": "Field values to set on all matching documents"
                    },
                    "hooks": {
                        "type": "boolean",
                        "description": "Run per-document lifecycle hooks (default: true)"
                    },
                    "draft": {
                        "type": "boolean",
                        "description": "Target draft versions (default: false)"
                    }
                },
                "required": ["data"]
            })
        }
        CrudOp::Delete | CrudOp::Undelete | CrudOp::Unpublish => id_only_schema(),
        CrudOp::DeleteMany => json!({
            "type": "object",
            "properties": {
                "where": {
                    "type": "object",
                    "description": "Filter conditions. Keys are field names, values are filter objects (e.g. {\"equals\": \"value\"}, {\"contains\": \"text\"}). Omit to match all documents."
                },
                "hooks": {
                    "type": "boolean",
                    "description": "Run per-document lifecycle hooks (default: true)"
                },
                "force_hard_delete": {
                    "type": "boolean",
                    "description": "Force hard delete even on soft-delete collections (default: false)"
                }
            }
        }),
        CrudOp::FindById => json!({
            "type": "object",
            "properties": {
                "id": { "type": "string" },
                "depth": { "type": "integer", "description": "Relationship population depth" },
                "locale": { "type": "string", "description": "Locale code (e.g. 'en', 'de') or 'all' for all locales" }
            },
            "required": ["id"]
        }),
        CrudOp::Find => json!({
            "type": "object",
            "properties": {
                "where": {
                    "type": "object",
                    "description": "Filter conditions. Keys are field names, values are filter objects (e.g. {\"equals\": \"value\"}, {\"contains\": \"text\"}, {\"greater_than\": 5})"
                },
                "order_by": { "type": "string", "description": "Sort field (prefix with - for descending)" },
                "limit": { "type": "integer", "description": "Max results per page" },
                "page": { "type": "integer", "description": "Page number (1-indexed, page mode only)" },
                "after_cursor": { "type": "string", "description": "Forward cursor (cursor mode only, mutually exclusive with page and before_cursor)" },
                "before_cursor": { "type": "string", "description": "Backward cursor (cursor mode only, mutually exclusive with page and after_cursor)" },
                "depth": { "type": "integer", "description": "Relationship population depth" },
                "search": { "type": "string", "description": "Full-text search query" },
                "locale": { "type": "string", "description": "Locale code (e.g. 'en', 'de') or 'all' for all locales" },
                "trash": { "type": "boolean", "description": "When true, return only soft-deleted documents (trash view)" }
            }
        }),
        CrudOp::Count => json!({
            "type": "object",
            "properties": {
                "where": {
                    "type": "object",
                    "description": "Filter conditions. Keys are field names, values are filter objects (e.g. {\"equals\": \"value\"}, {\"contains\": \"text\"}, {\"greater_than\": 5})"
                },
                "draft": { "type": "boolean", "description": "Include draft/deleted documents in the count" }
            }
        }),
        CrudOp::ListVersions => json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Document ID to list versions for" },
                "limit": { "type": "integer", "description": "Max versions to return" },
                "offset": { "type": "integer", "description": "Number of versions to skip" }
            },
            "required": ["id"]
        }),
        CrudOp::RestoreVersion => json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Document ID to restore" },
                "version_id": { "type": "string", "description": "Version snapshot ID to restore from" }
            },
            "required": ["id", "version_id"]
        }),
    }
}

/// Generate the input schema for a global CRUD tool.
pub fn global_input_schema(def: &GlobalDefinition, op: CrudOp) -> Value {
    match op {
        CrudOp::Find => {
            // Read global — no params needed
            json!({ "type": "object", "properties": {} })
        }
        CrudOp::Update => fields_to_object_schema(&def.fields),
        _ => json!({ "type": "object", "properties": {} }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        collection::{Auth, CollectionDefinition, GlobalDefinition},
        field::{
            BlockDefinition, FieldAdmin, FieldTab, LocalizedString, McpFieldConfig,
            RelationshipConfig, SelectOption,
        },
    };

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn required_text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .required(true)
            .build()
    }

    #[test]
    fn text_field_schema() {
        let f = text_field("title");
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "string");
    }

    #[test]
    fn number_field_schema() {
        let f = FieldDefinition::builder("count", FieldType::Number).build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "number");
    }

    #[test]
    fn checkbox_field_schema() {
        let f = FieldDefinition::builder("active", FieldType::Checkbox).build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "boolean");
    }

    #[test]
    fn select_field_with_options() {
        let f = FieldDefinition::builder("status", FieldType::Select)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Draft".to_string()), "draft"),
                SelectOption::new(LocalizedString::Plain("Published".to_string()), "published"),
            ])
            .build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "string");
        let enm = s["enum"].as_array().unwrap();
        assert_eq!(enm.len(), 2);
    }

    #[test]
    fn date_field_has_format() {
        let f = FieldDefinition::builder("created", FieldType::Date).build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["format"], "date-time");
    }

    #[test]
    fn relationship_has_many() {
        let f = FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "array");
    }

    #[test]
    fn mcp_description_included() {
        let f = FieldDefinition::builder("status", FieldType::Text)
            .mcp(McpFieldConfig {
                description: Some("Publication status".to_string()),
            })
            .build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["description"], "Publication status");
    }

    #[test]
    fn admin_description_fallback() {
        let f = FieldDefinition::builder("status", FieldType::Text)
            .admin(
                FieldAdmin::builder()
                    .description(LocalizedString::Plain("Admin desc".to_string()))
                    .build(),
            )
            .build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["description"], "Admin desc");
    }

    #[test]
    fn collection_create_schema() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![required_text("title"), text_field("body")];
        let s = collection_input_schema(&def, CrudOp::Create);
        assert!(s["properties"]["title"].is_object());
        assert!(s["properties"]["body"].is_object());
        let req = s["required"].as_array().unwrap();
        assert!(req.contains(&Value::String("title".to_string())));
    }

    #[test]
    fn collection_update_schema_has_id() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![text_field("title")];
        let s = collection_input_schema(&def, CrudOp::Update);
        assert!(s["properties"]["id"].is_object());
        assert!(
            s["required"]
                .as_array()
                .unwrap()
                .contains(&Value::String("id".to_string()))
        );
    }

    #[test]
    fn collection_delete_schema() {
        let def = CollectionDefinition::new("posts");
        let s = collection_input_schema(&def, CrudOp::Delete);
        assert!(s["properties"]["id"].is_object());
    }

    #[test]
    fn collection_find_schema() {
        let def = CollectionDefinition::new("posts");
        let s = collection_input_schema(&def, CrudOp::Find);
        assert!(s["properties"]["where"].is_object());
        assert!(s["properties"]["limit"].is_object());
        assert!(s["properties"]["page"].is_object());
        assert!(s["properties"]["after_cursor"].is_object());
        assert!(s["properties"]["before_cursor"].is_object());
    }

    #[test]
    fn array_field_schema() {
        let f = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![text_field("label"), required_text("value")])
            .build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "array");
        assert!(s["items"]["properties"]["label"].is_object());
    }

    #[test]
    fn layout_fields_flattened() {
        let row = FieldDefinition::builder("row1", FieldType::Row)
            .fields(vec![text_field("first_name"), text_field("last_name")])
            .build();
        let mut def = CollectionDefinition::new("people");
        def.fields = vec![row];
        let s = collection_input_schema(&def, CrudOp::Create);
        // Row's children should be promoted
        assert!(s["properties"]["first_name"].is_object());
        assert!(s["properties"]["last_name"].is_object());
        // Row itself should not appear
        assert!(s["properties"]["row1"].is_null());
    }

    #[test]
    fn global_read_schema() {
        let mut def = GlobalDefinition::new("settings");
        def.fields = vec![text_field("site_name")];
        let s = global_input_schema(&def, CrudOp::Find);
        assert!(s["properties"].is_object());
    }

    #[test]
    fn global_update_schema() {
        let mut def = GlobalDefinition::new("settings");
        def.fields = vec![required_text("site_name")];
        let s = global_input_schema(&def, CrudOp::Update);
        assert!(s["properties"]["site_name"].is_object());
    }

    // ── field types not yet covered ────────────────────────────────────────

    #[test]
    fn textarea_field_schema() {
        let f = FieldDefinition::builder("body", FieldType::Textarea).build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "string");
    }

    #[test]
    fn email_field_schema() {
        let f = FieldDefinition::builder("email", FieldType::Email).build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "string");
    }

    #[test]
    fn code_field_schema() {
        let f = FieldDefinition::builder("snippet", FieldType::Code).build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "string");
    }

    #[test]
    fn richtext_field_schema() {
        let f = FieldDefinition::builder("content", FieldType::Richtext).build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "string");
    }

    #[test]
    fn json_field_schema() {
        let f = FieldDefinition::builder("metadata", FieldType::Json).build();
        let s = field_to_json_schema(&f);
        // Json fields use an empty schema ({}) — no type restriction
        assert!(s.is_object());
        assert!(s.get("type").is_none());
    }

    #[test]
    fn group_field_schema() {
        let f = FieldDefinition::builder("address", FieldType::Group)
            .fields(vec![text_field("street"), required_text("city")])
            .build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "object");
        assert!(s["properties"]["street"].is_object());
        assert!(s["properties"]["city"].is_object());
        // "city" is required
        let req = s["required"].as_array().unwrap();
        assert!(req.contains(&Value::String("city".to_string())));
    }

    #[test]
    fn radio_field_schema_with_options() {
        let f = FieldDefinition::builder("size", FieldType::Radio)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("S".to_string()), "s"),
                SelectOption::new(LocalizedString::Plain("M".to_string()), "m"),
                SelectOption::new(LocalizedString::Plain("L".to_string()), "l"),
            ])
            .build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "string");
        let enm = s["enum"].as_array().unwrap();
        assert_eq!(enm.len(), 3);
    }

    #[test]
    fn radio_field_schema_without_options() {
        let f = FieldDefinition::builder("mode", FieldType::Radio).build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "string");
        assert!(s.get("enum").is_none());
    }

    #[test]
    fn select_field_without_options() {
        let f = FieldDefinition::builder("cat", FieldType::Select).build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "string");
        assert!(s.get("enum").is_none());
    }

    #[test]
    fn select_field_has_many() {
        let f = FieldDefinition::builder("tags", FieldType::Select)
            .has_many(true)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("A".to_string()), "a"),
                SelectOption::new(LocalizedString::Plain("B".to_string()), "b"),
            ])
            .build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "array");
        assert!(s["items"]["enum"].is_array());
    }

    #[test]
    fn upload_field_single() {
        let f = FieldDefinition::builder("avatar", FieldType::Upload).build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "string");
    }

    #[test]
    fn upload_field_has_many() {
        let f = FieldDefinition::builder("images", FieldType::Upload)
            .relationship(RelationshipConfig::new("media", true))
            .build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "array");
    }

    #[test]
    fn relationship_single_no_config() {
        // has_many from has_many field, no relationship config
        let f = FieldDefinition::builder("author", FieldType::Relationship).build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "string");
    }

    #[test]
    fn relationship_has_many_via_field() {
        // has_many from has_many field, no relationship config
        let f = FieldDefinition::builder("categories", FieldType::Relationship)
            .has_many(true)
            .build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "array");
    }

    #[test]
    fn row_field_schema_is_empty_object() {
        // Row as standalone field_to_json_schema → empty object placeholder
        let f = FieldDefinition::builder("my_row", FieldType::Row)
            .fields(vec![text_field("a"), text_field("b")])
            .build();
        let s = field_to_json_schema(&f);
        assert!(s.is_object());
        // Empty schema placeholder (no type key)
        assert!(s.get("type").is_none());
    }

    #[test]
    fn collapsible_field_schema_is_empty_object() {
        let f = FieldDefinition::builder("my_collapsible", FieldType::Collapsible)
            .fields(vec![text_field("x")])
            .build();
        let s = field_to_json_schema(&f);
        assert!(s.is_object());
        assert!(s.get("type").is_none());
    }

    #[test]
    fn tabs_field_schema_is_empty_object() {
        let f = FieldDefinition::builder("my_tabs", FieldType::Tabs).build();
        let s = field_to_json_schema(&f);
        assert!(s.is_object());
        assert!(s.get("type").is_none());
    }

    #[test]
    fn join_field_schema_is_string() {
        let f = FieldDefinition::builder("related", FieldType::Join).build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "string");
    }

    #[test]
    fn blocks_empty_schema() {
        let f = FieldDefinition::builder("content", FieldType::Blocks).build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "array");
        // No items when no blocks defined
        assert!(s.get("items").is_none());
    }

    #[test]
    fn blocks_with_variants_schema() {
        let f = FieldDefinition::builder("layout", FieldType::Blocks)
            .blocks(vec![
                BlockDefinition::new("hero", vec![required_text("heading")]),
                BlockDefinition::new("cta", vec![text_field("label"), text_field("url")]),
            ])
            .build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "array");
        let one_of = s["items"]["oneOf"].as_array().unwrap();
        assert_eq!(one_of.len(), 2);
        // Both variants require "blockType"
        for variant in one_of {
            let req = variant["required"].as_array().unwrap();
            assert!(req.contains(&Value::String("blockType".to_string())));
        }
        // hero variant has "heading" property
        let hero = one_of
            .iter()
            .find(|v| v["properties"]["blockType"]["const"].as_str() == Some("hero"))
            .unwrap();
        assert!(hero["properties"]["heading"].is_object());
    }

    // ── Tabs layout flattening ─────────────────────────────────────────────

    #[test]
    fn tabs_fields_flattened_in_object_schema() {
        let tabs = FieldDefinition::builder("tabs", FieldType::Tabs)
            .tabs(vec![
                FieldTab::new(
                    "SEO",
                    vec![text_field("meta_title"), required_text("meta_desc")],
                ),
                FieldTab::new("Content", vec![text_field("body")]),
            ])
            .build();
        let mut def = CollectionDefinition::new("pages");
        def.fields = vec![tabs];
        let s = collection_input_schema(&def, CrudOp::Create);
        // Tab fields should be promoted to the root
        assert!(s["properties"]["meta_title"].is_object());
        assert!(s["properties"]["meta_desc"].is_object());
        assert!(s["properties"]["body"].is_object());
        // "meta_desc" is required
        let req = s["required"].as_array().unwrap();
        assert!(req.contains(&Value::String("meta_desc".to_string())));
        // The tabs container itself should not appear
        assert!(s["properties"]["tabs"].is_null());
    }

    #[test]
    fn collapsible_fields_flattened_in_object_schema() {
        let collapsible = FieldDefinition::builder("collapsible_section", FieldType::Collapsible)
            .fields(vec![
                text_field("internal_notes"),
                required_text("reference_code"),
            ])
            .build();
        let mut def = CollectionDefinition::new("orders");
        def.fields = vec![collapsible];
        let s = collection_input_schema(&def, CrudOp::Create);
        assert!(s["properties"]["internal_notes"].is_object());
        assert!(s["properties"]["reference_code"].is_object());
        let req = s["required"].as_array().unwrap();
        assert!(req.contains(&Value::String("reference_code".to_string())));
        // Container itself should not appear
        assert!(s["properties"]["collapsible_section"].is_null());
    }

    #[test]
    fn join_fields_skipped_in_object_schema() {
        let join = FieldDefinition::builder("comments", FieldType::Join).build();
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![text_field("title"), join];
        let s = collection_input_schema(&def, CrudOp::Create);
        // title appears but comments (Join) does not
        assert!(s["properties"]["title"].is_object());
        assert!(s["properties"]["comments"].is_null());
    }

    // ── auth collection schema ─────────────────────────────────────────────

    #[test]
    fn auth_collection_create_adds_password_field() {
        // Use a required field so the "required" array is already present in the schema,
        // allowing the auth code path to push "password" into it.
        let mut def = CollectionDefinition::new("users");
        def.fields = vec![required_text("email"), text_field("name")];
        def.auth = Some(Auth {
            enabled: true,
            ..Default::default()
        });
        let s = collection_input_schema(&def, CrudOp::Create);
        assert!(s["properties"]["password"].is_object());
        // password is appended to the existing required array
        let req = s["required"].as_array().unwrap();
        assert!(req.contains(&Value::String("password".to_string())));
    }

    #[test]
    fn auth_collection_update_adds_optional_password_field() {
        let mut def = CollectionDefinition::new("users");
        def.fields = vec![text_field("name")];
        def.auth = Some(Auth {
            enabled: true,
            ..Default::default()
        });
        let s = collection_input_schema(&def, CrudOp::Update);
        // password appears but is not required (optional change)
        assert!(s["properties"]["password"].is_object());
        assert!(
            s["properties"]["password"]["description"]
                .as_str()
                .unwrap()
                .contains("empty")
        );
        // Only "id" is required for update
        let req = s["required"].as_array().unwrap();
        assert!(req.contains(&Value::String("id".to_string())));
        assert!(!req.contains(&Value::String("password".to_string())));
    }

    // ── collection_input_schema: FindById ──────────────────────────────────

    #[test]
    fn collection_find_by_id_schema() {
        let def = CollectionDefinition::new("posts");
        let s = collection_input_schema(&def, CrudOp::FindById);
        assert!(s["properties"]["id"].is_object());
        assert!(s["properties"]["depth"].is_object());
        let req = s["required"].as_array().unwrap();
        assert!(req.contains(&Value::String("id".to_string())));
    }

    // ── global_input_schema: non-Find/Update arms ──────────────────────────

    #[test]
    fn global_input_schema_other_ops_return_empty() {
        let mut def = GlobalDefinition::new("settings");
        def.fields = vec![text_field("site_name")];
        // Delete, Create, FindById all fall through to the `_` arm → empty schema
        for op in &[CrudOp::Delete, CrudOp::Create, CrudOp::FindById] {
            let s = global_input_schema(&def, *op);
            assert!(
                s["properties"].is_object(),
                "op {:?} should return object with properties",
                op
            );
            // Should be empty properties
            assert_eq!(
                s["properties"].as_object().unwrap().len(),
                0,
                "op {:?} should have no properties",
                op
            );
        }
    }

    // ── array field: required sub-fields ──────────────────────────────────

    #[test]
    fn array_field_required_sub_fields() {
        let f = FieldDefinition::builder("options", FieldType::Array)
            .fields(vec![required_text("key"), text_field("value")])
            .build();
        let s = field_to_json_schema(&f);
        assert_eq!(s["type"], "array");
        assert!(s["items"]["properties"]["key"].is_object());
        assert!(s["items"]["properties"]["value"].is_object());
        let req = s["items"]["required"].as_array().unwrap();
        assert!(req.contains(&Value::String("key".to_string())));
        assert!(!req.contains(&Value::String("value".to_string())));
    }

    #[test]
    fn auth_collection_password_required_even_without_other_required_fields() {
        let mut def = CollectionDefinition::new("users");
        def.auth = Some(Auth::new(true));
        // Only optional fields — no required fields
        def.fields = vec![FieldDefinition::builder("bio", FieldType::Text).build()];

        let schema = collection_input_schema(&def, CrudOp::Create);
        let required = schema["required"]
            .as_array()
            .expect("required array should exist");
        assert!(
            required.contains(&Value::String("password".to_string())),
            "password should be in required even when no other fields are required"
        );
    }
}
