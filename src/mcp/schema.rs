//! JSON Schema generation from `FieldDefinition` and `CollectionDefinition`.

use serde_json::{Map, Value, json};

use crate::core::{CollectionDefinition, FieldDefinition, FieldType, GlobalDefinition};
use crate::service::op::wire::{self, OpWire, WireField, WireKind, WireSurfaces};

/// CRUD operation type, determines which fields are included/required in the schema.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::mcp) enum CrudOp {
    Create,
    CreateMany,
    Update,
    UpdateMany,
    Validate,
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

impl CrudOp {
    /// Every collection CRUD operation, in a stable order. The single list the
    /// tool builder and the tool-name parser both iterate, so a new op can't be
    /// emitted-but-unroutable (or parsed-but-never-emitted).
    pub(in crate::mcp) const ALL: &'static [CrudOp] = &[
        CrudOp::Create,
        CrudOp::CreateMany,
        CrudOp::Update,
        CrudOp::UpdateMany,
        CrudOp::Validate,
        CrudOp::Find,
        CrudOp::FindById,
        CrudOp::Delete,
        CrudOp::DeleteMany,
        CrudOp::Undelete,
        CrudOp::Unpublish,
        CrudOp::Count,
        CrudOp::ListVersions,
        CrudOp::RestoreVersion,
    ];

    /// Canonical operation key — matches the tool-name verb and the key used
    /// for per-operation MCP description overrides (`mcp.operations`).
    pub(in crate::mcp) fn name(self) -> &'static str {
        match self {
            CrudOp::Create => "create",
            CrudOp::CreateMany => "create_many",
            CrudOp::Update => "update",
            CrudOp::UpdateMany => "update_many",
            CrudOp::Validate => "validate",
            CrudOp::Find => "find",
            CrudOp::FindById => "find_by_id",
            CrudOp::Delete => "delete",
            CrudOp::DeleteMany => "delete_many",
            CrudOp::Undelete => "undelete",
            CrudOp::Unpublish => "unpublish",
            CrudOp::Count => "count",
            CrudOp::ListVersions => "list_versions",
            CrudOp::RestoreVersion => "restore_version",
        }
    }
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
        .map_or(field.has_many, |r| r.has_many);

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
pub(in crate::mcp) fn field_to_json_schema(field: &FieldDefinition) -> Value {
    let description = field.mcp.description.as_deref().or(field
        .admin
        .description
        .as_ref()
        .map(crate::core::LocalizedString::resolve_default));

    let mut schema = match field.field_type {
        FieldType::Text
        | FieldType::Textarea
        | FieldType::Email
        | FieldType::Code
        | FieldType::Richtext
        | FieldType::Join => json!({ "type": "string" }),
        FieldType::Date => json!({ "type": "string", "format": "date-time" }),
        FieldType::Number => json!({ "type": "number" }),
        FieldType::Checkbox => json!({ "type": "boolean" }),
        FieldType::Select | FieldType::Radio => select_radio_schema(field),
        FieldType::Relationship | FieldType::Upload => relationship_schema(field),
        FieldType::Array => {
            json!({ "type": "array", "items": fields_to_object_schema(&field.fields) })
        }
        FieldType::Blocks => blocks_schema(field),
        FieldType::Group => fields_to_object_schema(&field.fields),
        // Json has no schema constraint; Row/Collapsible/Tabs are pure layout wrappers.
        FieldType::Json | FieldType::Row | FieldType::Collapsible | FieldType::Tabs => json!({}),
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

/// Append `name` to the schema's `required` array (creating it if absent).
fn push_required(schema: &mut Value, name: &str) {
    let obj = schema.as_object_mut().expect("schema is object");
    let req = obj
        .entry("required")
        .or_insert_with(|| Value::Array(Vec::new()));

    if let Some(arr) = req.as_array_mut() {
        arr.push(Value::String(name.to_string()));
    }
}

/// JSON-schema property for one scalar wire field.
fn wire_prop(field: &WireField) -> Value {
    let mut prop = match field.kind {
        WireKind::Bool => json!({ "type": "boolean" }),
        WireKind::Int => json!({ "type": "integer" }),
        WireKind::Str | WireKind::Id | WireKind::Locale => json!({ "type": "string" }),
        WireKind::FilterMap => json!({ "type": "object" }),
        WireKind::Select => json!({ "type": "array", "items": { "type": "string" } }),
        WireKind::DataFields | WireKind::DataObject | WireKind::DocumentsArray => {
            unreachable!("def-dependent kinds are rendered by add_wire_props")
        }
    };

    if !field.doc.is_empty() {
        let obj = prop.as_object_mut().expect("json! object");
        obj.insert(
            "description".to_string(),
            Value::String(field.doc.to_string()),
        );
    }

    prop
}

/// Render an op's MCP-visible wire fields into `schema`. The
/// [`WireKind::DataFields`] spread is the caller's *starting* schema
/// (`fields_to_object_schema`), so it is skipped here; the other
/// def-dependent kinds pull the collection fields from `def`.
fn add_wire_props(schema: &mut Value, wire: &OpWire, def: Option<&CollectionDefinition>) {
    for field in wire.fields {
        if !field.surfaces.contains(WireSurfaces::MCP) {
            continue;
        }

        let prop = match field.kind {
            WireKind::DataFields => continue,
            WireKind::DataObject => {
                let def = def.expect("DataObject op carries a collection definition");
                json!({
                    "allOf": [fields_to_object_schema(&def.fields)],
                    "description": field.doc
                })
            }
            WireKind::DocumentsArray => {
                let def = def.expect("DocumentsArray op carries a collection definition");
                json!({
                    "type": "array",
                    "items": create_many_item_schema(def),
                    "description": field.doc
                })
            }
            _ => wire_prop(field),
        };

        if let Some(props) = get_props(schema) {
            props.insert(field.name.to_string(), prop);
        }

        if field.required {
            push_required(schema, field.name);
        }
    }
}

/// Schema for an op with no top-level field-data spread — an object holding
/// exactly the wire model's option fields.
fn options_schema(wire: &OpWire, def: Option<&CollectionDefinition>) -> Value {
    let mut schema = json!({ "type": "object", "properties": {} });

    add_wire_props(&mut schema, wire, def);

    schema
}

/// Schema for an op whose field data spreads at the top level (create /
/// update / validate): the definition's field schema plus the wire options.
fn data_spread_schema(fields: &[FieldDefinition], wire: &OpWire) -> Value {
    let mut schema = fields_to_object_schema(fields);

    add_wire_props(&mut schema, wire, None);

    schema
}

/// Per-item schema for `create_many` — field data plus, for auth collections,
/// an OPTIONAL `password` (validated against the password policy by the service
/// create chokepoint when supplied). Unlike single `create`, items carry no
/// `locale`/`draft`/`events` (those are operation-level) and password is
/// optional, not required: bulk seeding may legitimately include strategy-only
/// users without a password.
fn create_many_item_schema(def: &CollectionDefinition) -> Value {
    let mut schema = fields_to_object_schema(&def.fields);

    if def.is_auth_collection()
        && let Some(props) = get_props(&mut schema)
    {
        props.insert(
            "password".to_string(),
            json!({
                "type": "string",
                "description": "Optional; validated against the password policy when set"
            }),
        );
    }

    schema
}

impl CrudOp {
    /// The op's wire-model entry — the single source for its option fields.
    fn wire(self) -> &'static OpWire {
        wire::collection_op(self.name())
            .unwrap_or_else(|| panic!("wire model missing collection op `{}`", self.name()))
    }
}

/// Generate the input schema for a collection CRUD tool. The option fields
/// come from the wire model ([`crate::service::op::wire`]); only the
/// def-dependent parts (field-data spread, auth `password` rules, the
/// partial-update required policy) remain per-op code here.
pub(in crate::mcp) fn collection_input_schema(def: &CollectionDefinition, op: CrudOp) -> Value {
    let wire = op.wire();

    match op {
        CrudOp::Create => {
            let mut schema = data_spread_schema(&def.fields, wire);

            // Auth collections take a required top-level `password` (hashed by
            // the service create chokepoint, never stored as field data).
            if def.is_auth_collection() {
                if let Some(props) = get_props(&mut schema) {
                    props.insert("password".to_string(), json!({ "type": "string" }));
                }
                push_required(&mut schema, "password");
            }

            schema
        }
        CrudOp::Update => {
            let mut schema = data_spread_schema(&def.fields, wire);

            if def.is_auth_collection()
                && let Some(props) = get_props(&mut schema)
            {
                props.insert(
                    "password".to_string(),
                    json!({
                        "type": "string",
                        "description": "Leave empty to keep current password"
                    }),
                );
            }

            // Partial update: field-level `required` constraints don't apply —
            // only the wire model's required options (`id`) do.
            let obj = schema.as_object_mut().expect("schema is object");
            obj.insert("required".to_string(), json!(["id"]));

            schema
        }
        CrudOp::Validate => data_spread_schema(&def.fields, wire),
        CrudOp::CreateMany | CrudOp::UpdateMany => options_schema(wire, Some(def)),
        CrudOp::Find
        | CrudOp::FindById
        | CrudOp::Count
        | CrudOp::Delete
        | CrudOp::DeleteMany
        | CrudOp::Undelete
        | CrudOp::Unpublish
        | CrudOp::ListVersions
        | CrudOp::RestoreVersion => options_schema(wire, None),
    }
}

/// Generate the input schema for a global CRUD tool — same wire-model
/// rendering, keyed by the global op names.
pub(in crate::mcp) fn global_input_schema(def: &GlobalDefinition, op: CrudOp) -> Value {
    let name = match op {
        CrudOp::Find => "get_global",
        CrudOp::Update => "update_global",
        CrudOp::Validate => "validate_global",
        _ => return json!({ "type": "object", "properties": {} }),
    };

    let wire =
        wire::global_op(name).unwrap_or_else(|| panic!("wire model missing global op `{name}`"));

    match op {
        CrudOp::Find => options_schema(wire, None),
        _ => data_spread_schema(&def.fields, wire),
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
    fn collection_validate_schema_optional_id() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![required_text("title"), text_field("body")];
        let s = collection_input_schema(&def, CrudOp::Validate);
        // Field-level requireds still apply...
        let req = s["required"].as_array().unwrap();
        assert!(req.contains(&Value::String("title".to_string())));
        // ...but `id` is offered and stays optional (create-or-update mode).
        assert!(s["properties"]["id"].is_object());
        assert!(!req.contains(&Value::String("id".to_string())));
        // Reserved write meta-keys are present.
        assert!(s["properties"]["locale"].is_object());
        assert!(s["properties"]["draft"].is_object());
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

    /// Regression (cross-surface harmonization): `create_many` now accepts a
    /// per-item password on auth collections (relaxed + policed at the service),
    /// so its item schema advertises an OPTIONAL `password` — not required, since
    /// bulk seeding may include strategy-only users. Keeps schema and handler in
    /// sync.
    #[test]
    fn auth_collection_create_many_item_advertises_optional_password() {
        let mut def = CollectionDefinition::new("users");
        def.fields = vec![required_text("email"), text_field("name")];
        def.auth = Some(Auth {
            enabled: true,
            ..Default::default()
        });
        let s = collection_input_schema(&def, CrudOp::CreateMany);
        let item = &s["properties"]["documents"]["items"];
        assert!(
            item["properties"]["password"].is_object(),
            "create_many item must advertise a password field for auth collections"
        );
        // Optional, NOT required (unlike single create).
        let item_required = item["required"].as_array();
        assert!(
            item_required.is_none_or(|r| !r.contains(&Value::String("password".to_string()))),
            "create_many password must be optional per item"
        );
    }

    /// A non-auth collection's `create_many` items carry no injected `password`.
    #[test]
    fn non_auth_create_many_item_has_no_password() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![required_text("title")];
        let s = collection_input_schema(&def, CrudOp::CreateMany);
        let item = &s["properties"]["documents"]["items"];
        assert!(item["properties"]["password"].is_null());
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
                "op {op:?} should return object with properties"
            );
            // Should be empty properties
            assert_eq!(
                s["properties"].as_object().unwrap().len(),
                0,
                "op {op:?} should have no properties"
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

    /// Regression (single-source wire model): the MCP `update_global` schema
    /// never advertised `draft` even though the codec accepts it — one of the
    /// hand-written-schema drift bugs the wire model exists to end. The model
    /// declares it once; the schema now renders it.
    #[test]
    fn global_update_schema_advertises_draft() {
        let mut def = GlobalDefinition::new("settings");
        def.fields = vec![text_field("site_name")];
        let s = global_input_schema(&def, CrudOp::Update);
        assert!(s["properties"]["draft"].is_object());
        assert!(s["properties"]["locale"].is_object());
        assert!(s["properties"]["events"].is_object());
    }

    /// `delete_many`'s `trash` flag (empty-the-trash) is declared Lua-only in
    /// the wire model — the MCP schema must not render it.
    #[test]
    fn delete_many_schema_omits_lua_only_trash() {
        let def = CollectionDefinition::new("posts");
        let s = collection_input_schema(&def, CrudOp::DeleteMany);
        assert!(s["properties"]["trash"].is_null());
        assert!(s["properties"]["where"].is_object());
        assert!(s["properties"]["force_hard_delete"].is_object());
        assert!(s["properties"]["events"].is_object());
    }

    /// Every collection `CrudOp` resolves to a wire-model entry — a new op
    /// can't be added to the enum without declaring its wire fields.
    #[test]
    fn every_crud_op_has_a_wire_entry() {
        for op in CrudOp::ALL {
            let _ = op.wire();
        }
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
