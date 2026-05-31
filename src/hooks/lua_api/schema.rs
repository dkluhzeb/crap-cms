//! `crap.schema` namespace — read-only schema introspection.

use std::sync::Arc;

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, LuaSerdeExt, Result as LuaResult, Table, Value};
use serde::Serialize;

use crate::core::{
    CollectionDefinition, FieldDefinition, GlobalDefinition, Labels, PickerAppearance, Registry,
    RegistryRead, SharedRegistry,
};
use crate::typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

// ── Projection structs ───────────────────────────────────────────────
//
// The introspection API returns a denormalized, user-friendly shape
// that doesn't map 1:1 to `CollectionDefinition` / `FieldDefinition`.
// Rather than building Lua tables ad-hoc, we project the registry types
// into these `Serialize` structs and let `lua.to_value()` produce the
// table — single source of truth for both Rust and the generated
// `crap.SchemaCollection` / `crap.SchemaField` Lua classes.

/// Labels block emitted on collections, globals, and list entries.
#[derive(Serialize)]
pub(crate) struct SchemaLabels {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) singular: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) plural: Option<String>,
}

/// `{ slug, labels }` summary returned by `list_collections` / `list_globals`.
#[derive(Serialize)]
struct SchemaSummary {
    slug: String,
    labels: SchemaLabels,
}

/// Result of `crap.schema.get_collection` / `crap.schema.get_global`.
/// Globals reuse the same shape with `has_*` flags set to false.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.SchemaCollection")]
pub(crate) struct SchemaCollection {
    /// Collection or global slug.
    pub(crate) slug: String,
    /// Localized labels (singular/plural, resolved to the default locale).
    #[lua(ty = "{ singular?: string, plural?: string }")]
    pub(crate) labels: SchemaLabels,
    /// Whether the collection auto-manages `created_at` / `updated_at`.
    pub(crate) timestamps: bool,
    /// Whether the collection is an auth collection.
    pub(crate) has_auth: bool,
    /// Whether the collection is an upload collection.
    pub(crate) has_upload: bool,
    /// Whether the collection has versioning enabled.
    pub(crate) has_versions: bool,
    /// Whether the collection has draft support.
    pub(crate) has_drafts: bool,
    /// Field definitions, in declaration order.
    #[lua(ty = "crap.SchemaField[]")]
    pub(crate) fields: Vec<SchemaField>,
}

/// `relationship` sub-table on a `SchemaField`. `collection` is a
/// `string` (non-polymorphic) or `string[]` (polymorphic) — modeled as
/// an untagged enum so serde picks the right serialization.
#[derive(Serialize)]
#[serde(untagged)]
enum SchemaRelationshipCollection {
    Single(String),
    Multi(Vec<String>),
}

#[derive(Serialize)]
struct SchemaRelationship {
    collection: SchemaRelationshipCollection,
    has_many: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_depth: Option<i32>,
}

/// One option in a `Select` / `Radio` field's `options` array.
#[derive(Serialize)]
struct SchemaOption {
    label: String,
    value: String,
}

/// One block definition under a `Blocks` field. Note: emitted key is
/// `type`, hence the `#[serde(rename)]`.
#[derive(Serialize)]
struct SchemaBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_url: Option<String>,
    fields: Vec<SchemaField>,
}

/// Admin-UI hints exposed by `SchemaField.admin`. Mirrors the subset
/// of `FieldAdmin` that the schema-read surface exposes (language,
/// features list, picker UI). Skipped entirely from the emitted Lua
/// table when every field is absent — see `is_empty()`.
#[derive(Serialize, Default)]
struct SchemaAdmin {
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    features: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    picker: Option<String>,
}

impl SchemaAdmin {
    fn is_empty(&self) -> bool {
        self.language.is_none() && self.features.is_empty() && self.picker.is_none()
    }
}

/// `Join` field configuration (`{ collection, on }`). `None` for every
/// non-`Join` field; always populated for `Join` fields.
#[derive(Serialize)]
struct SchemaJoin {
    collection: String,
    on: String,
}

/// One field in a `SchemaCollection`. The same shape covers all field
/// types — keys not relevant to a given type are simply omitted.
///
/// Nesting mirrors the *write* shape on `crap.FieldDefinition` so the
/// schema-read surface reads back the way the user wrote it:
/// admin-UI hints live under `admin`, join config lives under `join`,
/// relationship config under `relationship`.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.SchemaField")]
pub(crate) struct SchemaField {
    pub(crate) name: String,
    #[serde(rename = "type")]
    #[lua(rename = "type")]
    pub(crate) field_type: String,
    pub(crate) required: bool,
    pub(crate) localized: bool,
    pub(crate) unique: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(
        ty = "{ collection: string | string[], has_many: boolean, max_depth?: integer }",
        optional
    )]
    relationship: Option<SchemaRelationship>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    min_length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    max_length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    max: Option<f64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[lua(optional)]
    integer: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[lua(optional)]
    has_many: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    min_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    max_date: Option<String>,
    /// `Date`-field input type. Matches the user's
    /// `crap.FieldDefinition.picker_appearance` value (`"dayOnly"`,
    /// `"dayAndTime"`, …); absent for non-date fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "crap.PickerAppearance", optional)]
    picker_appearance: Option<PickerAppearance>,
    /// Admin-UI hints: `language` / `features` / `picker`. Nested to
    /// mirror the write-side `crap.FieldAdmin` shape — the user writes
    /// `admin = { picker = "card" }` and reads back `field.admin.picker`.
    #[serde(skip_serializing_if = "SchemaAdmin::is_empty")]
    #[lua(
        ty = "{ language?: string, features?: string[], picker?: string }",
        optional
    )]
    admin: SchemaAdmin,
    /// `Join` field config. Absent for every non-`Join` field; for
    /// `Join` fields, mirrors the user's `crap.FieldDefinition.join`
    /// value (`{ collection, on }`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "{ collection: string, on: string }", optional)]
    join: Option<SchemaJoin>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[lua(ty = "{ label: string, value: string }[]", optional)]
    options: Vec<SchemaOption>,
    /// Sub-fields for `Group` / `Array` field types (recursive).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[lua(ty = "crap.SchemaField[]", optional)]
    fields: Vec<SchemaField>,
    /// Block definitions for `Blocks` field types.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[lua(
        ty = "{ type: string, label?: string, group?: string, image_url?: string, fields: crap.SchemaField[] }[]",
        optional
    )]
    blocks: Vec<SchemaBlock>,
}

// ── Builders (CollectionDefinition / FieldDefinition → projection) ───

fn build_labels(labels: &Labels) -> SchemaLabels {
    SchemaLabels {
        singular: labels
            .singular
            .as_ref()
            .map(|s| s.resolve_default().to_string()),
        plural: labels
            .plural
            .as_ref()
            .map(|s| s.resolve_default().to_string()),
    }
}

fn build_summary(slug: &str, labels: &Labels) -> SchemaSummary {
    SchemaSummary {
        slug: slug.to_string(),
        labels: build_labels(labels),
    }
}

fn build_collection(def: &CollectionDefinition) -> SchemaCollection {
    SchemaCollection {
        slug: def.slug.to_string(),
        labels: build_labels(&def.labels),
        timestamps: def.timestamps,
        has_auth: def.is_auth_collection(),
        has_upload: def.is_upload_collection(),
        has_versions: def.has_versions(),
        has_drafts: def.has_drafts(),
        fields: def.fields.iter().map(build_field).collect(),
    }
}

fn build_global(def: &GlobalDefinition) -> SchemaCollection {
    SchemaCollection {
        slug: def.slug.to_string(),
        labels: build_labels(&def.labels),
        timestamps: false,
        has_auth: false,
        has_upload: false,
        has_versions: false,
        has_drafts: false,
        fields: def.fields.iter().map(build_field).collect(),
    }
}

fn build_relationship(f: &FieldDefinition) -> Option<SchemaRelationship> {
    let rc = f.relationship.as_ref()?;

    let collection = if rc.is_polymorphic() {
        SchemaRelationshipCollection::Multi(
            rc.polymorphic
                .iter()
                .map(|s| AsRef::<str>::as_ref(s).to_owned())
                .collect(),
        )
    } else {
        SchemaRelationshipCollection::Single(AsRef::<str>::as_ref(&rc.collection).to_owned())
    };

    Some(SchemaRelationship {
        collection,
        has_many: rc.has_many,
        max_depth: rc.max_depth,
    })
}

fn build_field(f: &FieldDefinition) -> SchemaField {
    SchemaField {
        name: f.name.clone(),
        field_type: f.field_type.as_str().to_owned(),
        required: f.required,
        localized: f.localized,
        unique: f.unique,
        relationship: build_relationship(f),
        min_length: f.min_length,
        max_length: f.max_length,
        min: f.min,
        max: f.max,
        integer: f.integer,
        has_many: f.has_many,
        min_date: f.min_date.clone(),
        max_date: f.max_date.clone(),
        picker_appearance: f.picker_appearance.clone(),
        admin: SchemaAdmin {
            language: f.admin.language.clone(),
            features: f.admin.features.clone(),
            picker: f.admin.picker.clone(),
        },
        join: f.join.as_ref().map(|j| SchemaJoin {
            collection: AsRef::<str>::as_ref(&j.collection).to_owned(),
            on: j.on.clone(),
        }),
        options: f
            .options
            .iter()
            .map(|o| SchemaOption {
                label: o.label.resolve_default().to_owned(),
                value: o.value.clone(),
            })
            .collect(),
        fields: f.fields.iter().map(build_field).collect(),
        blocks: f
            .blocks
            .iter()
            .map(|b| SchemaBlock {
                block_type: b.block_type.clone(),
                label: b.label.as_ref().map(|s| s.resolve_default().to_owned()),
                group: b.group.clone(),
                image_url: b.image_url.clone(),
                fields: b.fields.iter().map(build_field).collect(),
            })
            .collect(),
    }
}

// ── Lua-side getters ─────────────────────────────────────────────────

fn get_collection(lua: &Lua, registry: &Registry, slug: &str) -> LuaResult<Value> {
    let Some(def) = registry.get_collection(slug) else {
        return Ok(Value::Nil);
    };
    lua.to_value(&build_collection(def))
}

fn get_global(lua: &Lua, registry: &Registry, slug: &str) -> LuaResult<Value> {
    let Some(def) = registry.get_global(slug) else {
        return Ok(Value::Nil);
    };
    lua.to_value(&build_global(def))
}

fn list_collections_fn(lua: &Lua, registry: &Registry) -> LuaResult<Table> {
    let summaries: Vec<SchemaSummary> = registry
        .collections
        .values()
        .map(|d| build_summary(&d.slug, &d.labels))
        .collect();

    let Value::Table(tbl) = lua.to_value(&summaries)? else {
        return Err(RuntimeError(
            "list_collections did not serialize to a table".into(),
        ));
    };
    Ok(tbl)
}

fn list_globals_fn(lua: &Lua, registry: &Registry) -> LuaResult<Table> {
    let summaries: Vec<SchemaSummary> = registry
        .globals
        .values()
        .map(|d| build_summary(&d.slug, &d.labels))
        .collect();

    let Value::Table(tbl) = lua.to_value(&summaries)? else {
        return Err(RuntimeError(
            "list_globals did not serialize to a table".into(),
        ));
    };
    Ok(tbl)
}

// ── User-facing fns (init/pool variants) ─────────────────────────────
//
// Four fns × init/pool = 8 `#[lua_fn]` decls. Like `access.rs`, the
// `lua_table!` macro takes a concrete state type, so the generic
// `RegistryRead` impl is split into two concrete-state variants
// (`SharedRegistry` for init VMs, `Arc<Registry>` for pool VMs).

/// Get a collection's schema definition.
#[lua_fn(
    path = "crap.schema.get_collection",
    returns = "crap.SchemaCollection?"
)]
fn schema_get_collection_init(
    state: &SharedRegistry,
    lua: &Lua,
    #[lua(doc = "Collection slug.")] slug: String,
) -> LuaResult<Value> {
    state
        .with(|r| get_collection(lua, r, &slug))
        .map_err(|e| RuntimeError(e.to_string()))?
}

#[lua_fn(
    path = "crap.schema.get_collection",
    returns = "crap.SchemaCollection?"
)]
fn schema_get_collection_pool(state: &Arc<Registry>, lua: &Lua, slug: String) -> LuaResult<Value> {
    state
        .with(|r| get_collection(lua, r, &slug))
        .map_err(|e| RuntimeError(e.to_string()))?
}

/// Get a global's schema definition.
#[lua_fn(path = "crap.schema.get_global", returns = "crap.SchemaCollection?")]
fn schema_get_global_init(
    state: &SharedRegistry,
    lua: &Lua,
    #[lua(doc = "Global slug.")] slug: String,
) -> LuaResult<Value> {
    state
        .with(|r| get_global(lua, r, &slug))
        .map_err(|e| RuntimeError(e.to_string()))?
}

#[lua_fn(path = "crap.schema.get_global", returns = "crap.SchemaCollection?")]
fn schema_get_global_pool(state: &Arc<Registry>, lua: &Lua, slug: String) -> LuaResult<Value> {
    state
        .with(|r| get_global(lua, r, &slug))
        .map_err(|e| RuntimeError(e.to_string()))?
}

/// List all collection slugs and labels.
#[lua_fn(
    path = "crap.schema.list_collections",
    returns = "{ slug: string, labels: { singular?: string, plural?: string } }[]"
)]
fn schema_list_collections_init(state: &SharedRegistry, lua: &Lua) -> LuaResult<Table> {
    state
        .with(|r| list_collections_fn(lua, r))
        .map_err(|e| RuntimeError(e.to_string()))?
}

#[lua_fn(
    path = "crap.schema.list_collections",
    returns = "{ slug: string, labels: { singular?: string, plural?: string } }[]"
)]
fn schema_list_collections_pool(state: &Arc<Registry>, lua: &Lua) -> LuaResult<Table> {
    state
        .with(|r| list_collections_fn(lua, r))
        .map_err(|e| RuntimeError(e.to_string()))?
}

/// List all global slugs and labels.
#[lua_fn(
    path = "crap.schema.list_globals",
    returns = "{ slug: string, labels: { singular?: string, plural?: string } }[]"
)]
fn schema_list_globals_init(state: &SharedRegistry, lua: &Lua) -> LuaResult<Table> {
    state
        .with(|r| list_globals_fn(lua, r))
        .map_err(|e| RuntimeError(e.to_string()))?
}

#[lua_fn(
    path = "crap.schema.list_globals",
    returns = "{ slug: string, labels: { singular?: string, plural?: string } }[]"
)]
fn schema_list_globals_pool(state: &Arc<Registry>, lua: &Lua) -> LuaResult<Table> {
    state
        .with(|r| list_globals_fn(lua, r))
        .map_err(|e| RuntimeError(e.to_string()))?
}

lua_table! {
    name: crap_schema_init,
    path: "crap.schema",
    state: SharedRegistry,
    header: "Schema introspection API (read-only). Reads from the loaded registry.",
    fns: [
        schema_get_collection_init,
        schema_get_global_init,
        schema_list_collections_init,
        schema_list_globals_init,
    ],
}

lua_table! {
    name: crap_schema_pool,
    path: "crap.schema",
    state: Arc<Registry>,
    fns: [
        schema_get_collection_pool,
        schema_get_global_pool,
        schema_list_collections_pool,
        schema_list_globals_pool,
    ],
}

/// Register `crap.schema` for init-phase VMs.
pub(super) fn register_schema_init(lua: &Lua, registry: SharedRegistry) -> Result<()> {
    register_crap_schema_init(lua, registry)?;
    Ok(())
}

/// Register `crap.schema` for pool VMs.
pub(super) fn register_schema_pool(lua: &Lua, registry: Arc<Registry>) -> Result<()> {
    register_crap_schema_pool(lua, registry)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        CollectionDefinition, Registry,
        collection::GlobalDefinition,
        field::{
            BlockDefinition, FieldDefinition, FieldType, JoinConfig, LocalizedString,
            RelationshipConfig,
        },
    };
    use mlua::Lua;
    use std::sync::Arc;

    fn make_registry_with_collection() -> Arc<Registry> {
        let mut reg = Registry::new();
        let mut posts = CollectionDefinition::new("posts");
        posts.labels.singular = Some(LocalizedString::Plain("Post".to_string()));
        posts.labels.plural = Some(LocalizedString::Plain("Posts".to_string()));
        posts.timestamps = true;
        let mut tags_rel = RelationshipConfig::new("tags", true);
        tags_rel.max_depth = Some(1);
        posts.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .build(),
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(tags_rel)
                .build(),
        ];
        reg.register_collection(posts);
        let mut settings = GlobalDefinition::new("settings");
        settings.labels.singular = Some(LocalizedString::Plain("Setting".to_string()));
        settings.fields = vec![FieldDefinition::builder("site_name", FieldType::Text).build()];
        reg.register_global(settings);
        Arc::new(reg)
    }

    fn to_table(_lua: &Lua, value: Value) -> Table {
        match value {
            Value::Table(t) => t,
            other => panic!("expected Table, got {other:?}"),
        }
    }

    #[test]
    fn build_collection_basic() {
        let reg = make_registry_with_collection();
        let def = reg.get_collection("posts").unwrap();
        let sc = build_collection(def);

        assert_eq!(sc.slug, "posts");
        assert!(sc.timestamps);
        assert!(!sc.has_auth);
        assert_eq!(sc.labels.singular.as_deref(), Some("Post"));
        assert_eq!(sc.labels.plural.as_deref(), Some("Posts"));
        assert_eq!(sc.fields.len(), 2);
        assert_eq!(sc.fields[0].name, "title");
        assert!(sc.fields[0].required);
    }

    #[test]
    fn build_field_with_relationship() {
        let reg = make_registry_with_collection();
        let def = reg.get_collection("posts").unwrap();
        let tags_field = &def.fields[1];
        let sf = build_field(tags_field);

        assert_eq!(sf.name, "tags");
        assert_eq!(sf.field_type, "relationship");
        let rel = sf.relationship.unwrap();
        match rel.collection {
            SchemaRelationshipCollection::Single(s) => assert_eq!(s, "tags"),
            SchemaRelationshipCollection::Multi(_) => panic!("expected non-polymorphic"),
        }
        assert!(rel.has_many);
        assert_eq!(rel.max_depth, Some(1));
    }

    #[test]
    fn register_schema_get_collection() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        lua.globals().set("crap", crap.clone()).unwrap();
        let reg = make_registry_with_collection();
        register_schema_pool(&lua, reg).unwrap();

        let schema: Table = crap.get("schema").unwrap();
        let get_coll: mlua::Function = schema.get("get_collection").unwrap();
        let result: Value = get_coll.call("posts".to_string()).unwrap();
        assert!(matches!(result, Value::Table(_)));

        let not_found: Value = get_coll.call("nonexistent".to_string()).unwrap();
        assert!(matches!(not_found, Value::Nil));
    }

    #[test]
    fn register_schema_get_global() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        lua.globals().set("crap", crap.clone()).unwrap();
        let reg = make_registry_with_collection();
        register_schema_pool(&lua, reg).unwrap();

        let schema: Table = crap.get("schema").unwrap();
        let get_global: mlua::Function = schema.get("get_global").unwrap();
        let result: Value = get_global.call("settings".to_string()).unwrap();
        let tbl = to_table(&lua, result);
        let slug: String = tbl.get("slug").unwrap();
        assert_eq!(slug, "settings");

        let not_found: Value = get_global.call("nonexistent".to_string()).unwrap();
        assert!(matches!(not_found, Value::Nil));
    }

    #[test]
    fn register_schema_list_collections() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        lua.globals().set("crap", crap.clone()).unwrap();
        let reg = make_registry_with_collection();
        register_schema_pool(&lua, reg).unwrap();

        let schema: Table = crap.get("schema").unwrap();
        let list: mlua::Function = schema.get("list_collections").unwrap();
        let result: Table = list.call(()).unwrap();
        let first: Table = result.get(1).unwrap();
        let slug: String = first.get("slug").unwrap();
        assert_eq!(slug, "posts");
    }

    #[test]
    fn register_schema_list_globals() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        lua.globals().set("crap", crap.clone()).unwrap();
        let reg = make_registry_with_collection();
        register_schema_pool(&lua, reg).unwrap();

        let schema: Table = crap.get("schema").unwrap();
        let list: mlua::Function = schema.get("list_globals").unwrap();
        let result: Table = list.call(()).unwrap();
        let first: Table = result.get(1).unwrap();
        let slug: String = first.get("slug").unwrap();
        assert_eq!(slug, "settings");
    }

    #[test]
    fn polymorphic_relationship_emits_array() {
        let mut rel_cfg = RelationshipConfig::new("articles", true);
        rel_cfg.polymorphic = vec!["articles".into(), "pages".into()];
        let field = FieldDefinition::builder("refs", FieldType::Relationship)
            .relationship(rel_cfg)
            .build();
        let sf = build_field(&field);
        let rel = sf.relationship.unwrap();
        match rel.collection {
            SchemaRelationshipCollection::Multi(slugs) => {
                assert_eq!(slugs, vec!["articles".to_string(), "pages".to_string()]);
            }
            SchemaRelationshipCollection::Single(_) => panic!("expected polymorphic"),
        }
        assert!(rel.has_many);
    }

    #[test]
    fn non_polymorphic_relationship_emits_string() {
        let field = FieldDefinition::builder("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new("users", false))
            .build();
        let sf = build_field(&field);
        match sf.relationship.unwrap().collection {
            SchemaRelationshipCollection::Single(s) => assert_eq!(s, "users"),
            SchemaRelationshipCollection::Multi(_) => panic!("expected single"),
        }
    }

    #[test]
    fn richtext_features_emit_array_under_admin() {
        let mut field = FieldDefinition::builder("body", FieldType::Richtext).build();
        field.admin.features = vec![
            "bold".to_string(),
            "italic".to_string(),
            "heading".to_string(),
        ];
        let sf = build_field(&field);
        assert_eq!(sf.admin.features, vec!["bold", "italic", "heading"]);
    }

    #[test]
    fn richtext_no_features_omits_admin_key() {
        let field = FieldDefinition::builder("body", FieldType::Richtext).build();
        let sf = build_field(&field);
        assert!(sf.admin.features.is_empty());
        // Empty admin block doesn't serialize at all — see
        // `SchemaAdmin::is_empty`.
        let lua = Lua::new();
        let v = lua.to_value(&sf).unwrap();
        let tbl = to_table(&lua, v);
        assert!(matches!(tbl.get::<Value>("admin"), Ok(Value::Nil)));
    }

    #[test]
    fn min_max_length_and_value() {
        let field = FieldDefinition::builder("score", FieldType::Number)
            .min_length(2)
            .max_length(100)
            .min(0.5)
            .max(99.9)
            .build();
        let sf = build_field(&field);
        assert_eq!(sf.min_length, Some(2));
        assert_eq!(sf.max_length, Some(100));
        assert!((sf.min.unwrap() - 0.5).abs() < f64::EPSILON);
        assert!((sf.max.unwrap() - 99.9).abs() < 1e-9);
    }

    #[test]
    fn has_many_true_emitted() {
        let field = FieldDefinition::builder("tags", FieldType::Select)
            .has_many(true)
            .build();
        let sf = build_field(&field);
        assert!(sf.has_many);
    }

    #[test]
    fn min_max_date_emitted() {
        let field = FieldDefinition::builder("published_at", FieldType::Date)
            .min_date("2020-01-01")
            .max_date("2030-12-31")
            .build();
        let sf = build_field(&field);
        assert_eq!(sf.min_date.as_deref(), Some("2020-01-01"));
        assert_eq!(sf.max_date.as_deref(), Some("2030-12-31"));
    }

    #[test]
    fn admin_language_emitted() {
        let mut field = FieldDefinition::builder("snippet", FieldType::Code).build();
        field.admin.language = Some("javascript".to_string());
        let sf = build_field(&field);
        assert_eq!(sf.admin.language.as_deref(), Some("javascript"));
    }

    #[test]
    fn admin_picker_emitted() {
        let mut field = FieldDefinition::builder("layout", FieldType::Blocks).build();
        field.admin.picker = Some("card".to_string());
        let sf = build_field(&field);
        assert_eq!(sf.admin.picker.as_deref(), Some("card"));
    }

    #[test]
    fn picker_appearance_emitted_for_date_field() {
        let field = FieldDefinition::builder("published_at", FieldType::Date)
            .picker_appearance(PickerAppearance::DayAndTime)
            .build();
        let sf = build_field(&field);
        assert_eq!(sf.picker_appearance, Some(PickerAppearance::DayAndTime));

        // Round-trip through lua: emits as the camelCase string.
        let lua = Lua::new();
        let v = lua.to_value(&sf).unwrap();
        let tbl = to_table(&lua, v);
        assert_eq!(
            tbl.get::<String>("picker_appearance").unwrap(),
            "dayAndTime"
        );
    }

    #[test]
    fn join_config_emitted_nested() {
        let field = FieldDefinition::builder("comments", FieldType::Join)
            .join(JoinConfig::new("comments", "post_id"))
            .build();
        let sf = build_field(&field);
        let join = sf.join.as_ref().expect("join should be present");
        assert_eq!(join.collection, "comments");
        assert_eq!(join.on, "post_id");
    }

    #[test]
    fn non_join_field_omits_join_key() {
        let field = FieldDefinition::builder("title", FieldType::Text).build();
        let sf = build_field(&field);
        assert!(sf.join.is_none());

        let lua = Lua::new();
        let v = lua.to_value(&sf).unwrap();
        let tbl = to_table(&lua, v);
        assert!(matches!(tbl.get::<Value>("join"), Ok(Value::Nil)));
    }

    #[test]
    fn sub_fields_emitted() {
        let field = FieldDefinition::builder("address", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("street", FieldType::Text).build(),
                FieldDefinition::builder("city", FieldType::Text)
                    .required(true)
                    .build(),
            ])
            .build();
        let sf = build_field(&field);
        assert_eq!(sf.fields.len(), 2);
        assert_eq!(sf.fields[0].name, "street");
        assert_eq!(sf.fields[1].name, "city");
        assert!(sf.fields[1].required);
    }

    #[test]
    fn global_with_no_plural_label() {
        let mut reg = Registry::new();
        let mut branding = GlobalDefinition::new("branding");
        branding.labels.singular = Some(LocalizedString::Plain("Brand".to_string()));
        // no plural label
        reg.register_global(branding);
        let registry = Arc::new(reg);

        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        lua.globals().set("crap", crap.clone()).unwrap();
        register_schema_pool(&lua, registry).unwrap();

        let schema: Table = crap.get("schema").unwrap();
        let get_global: mlua::Function = schema.get("get_global").unwrap();
        let result: Value = get_global.call("branding".to_string()).unwrap();
        let tbl = to_table(&lua, result);
        let slug: String = tbl.get("slug").unwrap();
        assert_eq!(slug, "branding");
        let labels: Table = tbl.get("labels").unwrap();
        let singular: String = labels.get("singular").unwrap();
        assert_eq!(singular, "Brand");
        let plural: Value = labels.get("plural").unwrap();
        assert!(matches!(plural, Value::Nil));
    }

    #[test]
    fn blocks_with_group_and_image() {
        let field = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks({
                let mut hero = BlockDefinition::new("hero", vec![]);
                hero.label = Some(LocalizedString::Plain("Hero".to_string()));
                hero.group = Some("Layout".to_string());
                hero.image_url = Some("/static/blocks/hero.svg".to_string());
                let mut text_block = BlockDefinition::new("text", vec![]);
                text_block.label = Some(LocalizedString::Plain("Text".to_string()));
                text_block.group = Some("Content".to_string());
                let mut divider = BlockDefinition::new("divider", vec![]);
                divider.label = Some(LocalizedString::Plain("Divider".to_string()));
                vec![hero, text_block, divider]
            })
            .build();
        let sf = build_field(&field);
        assert_eq!(sf.blocks.len(), 3);
        assert_eq!(sf.blocks[0].block_type, "hero");
        assert_eq!(sf.blocks[0].group.as_deref(), Some("Layout"));
        assert_eq!(
            sf.blocks[0].image_url.as_deref(),
            Some("/static/blocks/hero.svg")
        );
        assert_eq!(sf.blocks[1].group.as_deref(), Some("Content"));
        assert!(sf.blocks[1].image_url.is_none());
        assert!(sf.blocks[2].group.is_none());
        assert!(sf.blocks[2].image_url.is_none());
    }
}
