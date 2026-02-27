//! `crap.schema` namespace — read-only schema introspection.

use anyhow::Result;
use mlua::{Lua, Table, Value};

use crate::core::SharedRegistry;

/// Register `crap.schema` — read-only collection/global introspection.
pub(super) fn register_schema(lua: &Lua, crap: &Table, registry: SharedRegistry) -> Result<()> {
    let schema_table = lua.create_table()?;

    let reg = registry.clone();
    let get_collection_fn = lua.create_function(move |lua, slug: String| -> mlua::Result<Value> {
        let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
            format!("Registry lock: {}", e)
        ))?;
        match r.get_collection(&slug) {
            Some(def) => Ok(Value::Table(collection_def_to_lua_table(lua, def)?)),
            None => Ok(Value::Nil),
        }
    })?;
    schema_table.set("get_collection", get_collection_fn)?;

    let reg = registry.clone();
    let get_global_fn = lua.create_function(move |lua, slug: String| -> mlua::Result<Value> {
        let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
            format!("Registry lock: {}", e)
        ))?;
        match r.get_global(&slug) {
            Some(def) => {
                let tbl = lua.create_table()?;
                tbl.set("slug", def.slug.as_str())?;
                let labels = lua.create_table()?;
                if let Some(ref s) = def.labels.singular {
                    labels.set("singular", s.resolve_default())?;
                }
                if let Some(ref s) = def.labels.plural {
                    labels.set("plural", s.resolve_default())?;
                }
                tbl.set("labels", labels)?;
                let fields_arr = lua.create_table()?;
                for (i, f) in def.fields.iter().enumerate() {
                    fields_arr.set(i + 1, field_def_to_lua_table(lua, f)?)?;
                }
                tbl.set("fields", fields_arr)?;
                Ok(Value::Table(tbl))
            }
            None => Ok(Value::Nil),
        }
    })?;
    schema_table.set("get_global", get_global_fn)?;

    let reg = registry.clone();
    let list_collections_fn = lua.create_function(move |lua, ()| -> mlua::Result<Table> {
        let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
            format!("Registry lock: {}", e)
        ))?;
        let tbl = lua.create_table()?;
        let mut i = 0;
        for def in r.collections.values() {
            i += 1;
            let item = lua.create_table()?;
            item.set("slug", def.slug.as_str())?;
            let labels = lua.create_table()?;
            if let Some(ref s) = def.labels.singular {
                labels.set("singular", s.resolve_default())?;
            }
            if let Some(ref s) = def.labels.plural {
                labels.set("plural", s.resolve_default())?;
            }
            item.set("labels", labels)?;
            tbl.set(i, item)?;
        }
        Ok(tbl)
    })?;
    schema_table.set("list_collections", list_collections_fn)?;

    let reg = registry.clone();
    let list_globals_fn = lua.create_function(move |lua, ()| -> mlua::Result<Table> {
        let r = reg.read().map_err(|e| mlua::Error::RuntimeError(
            format!("Registry lock: {}", e)
        ))?;
        let tbl = lua.create_table()?;
        let mut i = 0;
        for def in r.globals.values() {
            i += 1;
            let item = lua.create_table()?;
            item.set("slug", def.slug.as_str())?;
            let labels = lua.create_table()?;
            if let Some(ref s) = def.labels.singular {
                labels.set("singular", s.resolve_default())?;
            }
            if let Some(ref s) = def.labels.plural {
                labels.set("plural", s.resolve_default())?;
            }
            item.set("labels", labels)?;
            tbl.set(i, item)?;
        }
        Ok(tbl)
    })?;
    schema_table.set("list_globals", list_globals_fn)?;

    crap.set("schema", schema_table)?;

    Ok(())
}

/// Convert a CollectionDefinition to a Lua table for crap.schema.get_collection().
fn collection_def_to_lua_table(lua: &Lua, def: &crate::core::CollectionDefinition) -> mlua::Result<Table> {
    let tbl = lua.create_table()?;
    tbl.set("slug", def.slug.as_str())?;
    let labels = lua.create_table()?;
    if let Some(ref s) = def.labels.singular {
        labels.set("singular", s.resolve_default())?;
    }
    if let Some(ref s) = def.labels.plural {
        labels.set("plural", s.resolve_default())?;
    }
    tbl.set("labels", labels)?;
    tbl.set("timestamps", def.timestamps)?;
    tbl.set("has_auth", def.is_auth_collection())?;
    tbl.set("has_upload", def.is_upload_collection())?;
    tbl.set("has_versions", def.has_versions())?;
    tbl.set("has_drafts", def.has_drafts())?;

    let fields_arr = lua.create_table()?;
    for (i, f) in def.fields.iter().enumerate() {
        fields_arr.set(i + 1, field_def_to_lua_table(lua, f)?)?;
    }
    tbl.set("fields", fields_arr)?;
    Ok(tbl)
}

/// Convert a FieldDefinition to a Lua table for schema introspection.
fn field_def_to_lua_table(lua: &Lua, f: &crate::core::field::FieldDefinition) -> mlua::Result<Table> {
    let tbl = lua.create_table()?;
    tbl.set("name", f.name.as_str())?;
    tbl.set("type", f.field_type.as_str())?;
    tbl.set("required", f.required)?;
    tbl.set("localized", f.localized)?;
    tbl.set("unique", f.unique)?;

    if let Some(ref rc) = f.relationship {
        let rel = lua.create_table()?;
        rel.set("collection", rc.collection.as_str())?;
        rel.set("has_many", rc.has_many)?;
        if let Some(md) = rc.max_depth {
            rel.set("max_depth", md)?;
        }
        tbl.set("relationship", rel)?;
    }

    if !f.options.is_empty() {
        let opts = lua.create_table()?;
        for (i, opt) in f.options.iter().enumerate() {
            let o = lua.create_table()?;
            o.set("label", opt.label.resolve_default())?;
            o.set("value", opt.value.as_str())?;
            opts.set(i + 1, o)?;
        }
        tbl.set("options", opts)?;
    }

    // Recurse into sub-fields (array, group)
    if !f.fields.is_empty() {
        let sub = lua.create_table()?;
        for (i, sf) in f.fields.iter().enumerate() {
            sub.set(i + 1, field_def_to_lua_table(lua, sf)?)?;
        }
        tbl.set("fields", sub)?;
    }

    // Blocks
    if !f.blocks.is_empty() {
        let blocks = lua.create_table()?;
        for (i, b) in f.blocks.iter().enumerate() {
            let bt = lua.create_table()?;
            bt.set("type", b.block_type.as_str())?;
            if let Some(ref lbl) = b.label {
                bt.set("label", lbl.resolve_default())?;
            }
            let bf = lua.create_table()?;
            for (j, sf) in b.fields.iter().enumerate() {
                bf.set(j + 1, field_def_to_lua_table(lua, sf)?)?;
            }
            bt.set("fields", bf)?;
            blocks.set(i + 1, bt)?;
        }
        tbl.set("blocks", blocks)?;
    }

    Ok(tbl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;
    use std::sync::{Arc, RwLock};

    fn make_registry_with_collection() -> crate::core::SharedRegistry {
        let mut reg = crate::core::Registry::new();
        reg.register_collection(crate::core::CollectionDefinition {
            slug: "posts".to_string(),
            labels: crate::core::collection::CollectionLabels {
                singular: Some(crate::core::field::LocalizedString::Plain("Post".to_string())),
                plural: Some(crate::core::field::LocalizedString::Plain("Posts".to_string())),
            },
            timestamps: true,
            fields: vec![
                crate::core::field::FieldDefinition {
                    name: "title".to_string(),
                    field_type: crate::core::field::FieldType::Text,
                    required: true,
                    ..Default::default()
                },
                crate::core::field::FieldDefinition {
                    name: "tags".to_string(),
                    field_type: crate::core::field::FieldType::Relationship,
                    relationship: Some(crate::core::field::RelationshipConfig {
                        collection: "tags".to_string(),
                        has_many: true,
                        max_depth: Some(1),
                    }),
                    ..Default::default()
                },
            ],
            admin: crate::core::collection::CollectionAdmin::default(),
            hooks: crate::core::collection::CollectionHooks::default(),
            auth: None,
            upload: None,
            access: crate::core::collection::CollectionAccess::default(),
            live: None,
            versions: None,
        });
        reg.register_global(crate::core::collection::GlobalDefinition {
            slug: "settings".to_string(),
            labels: crate::core::collection::CollectionLabels {
                singular: Some(crate::core::field::LocalizedString::Plain("Setting".to_string())),
                plural: None,
            },
            fields: vec![
                crate::core::field::FieldDefinition {
                    name: "site_name".to_string(),
                    ..Default::default()
                },
            ],
            hooks: crate::core::collection::CollectionHooks::default(),
            access: crate::core::collection::CollectionAccess::default(),
            live: None,
            versions: None,
        });
        Arc::new(RwLock::new(reg))
    }

    #[test]
    fn collection_def_to_lua_table_basic() {
        let lua = Lua::new();
        let reg = make_registry_with_collection();
        let r = reg.read().unwrap();
        let def = r.get_collection("posts").unwrap();
        let tbl = collection_def_to_lua_table(&lua, def).unwrap();

        let slug: String = tbl.get("slug").unwrap();
        assert_eq!(slug, "posts");
        let timestamps: bool = tbl.get("timestamps").unwrap();
        assert!(timestamps);
        let has_auth: bool = tbl.get("has_auth").unwrap();
        assert!(!has_auth);

        let labels: Table = tbl.get("labels").unwrap();
        let singular: String = labels.get("singular").unwrap();
        assert_eq!(singular, "Post");

        let fields: Table = tbl.get("fields").unwrap();
        let f1: Table = fields.get(1).unwrap();
        let name: String = f1.get("name").unwrap();
        assert_eq!(name, "title");
        let required: bool = f1.get("required").unwrap();
        assert!(required);
    }

    #[test]
    fn field_def_to_lua_table_with_relationship() {
        let lua = Lua::new();
        let reg = make_registry_with_collection();
        let r = reg.read().unwrap();
        let def = r.get_collection("posts").unwrap();
        let tags_field = &def.fields[1];
        let tbl = field_def_to_lua_table(&lua, tags_field).unwrap();

        let name: String = tbl.get("name").unwrap();
        assert_eq!(name, "tags");
        let ft: String = tbl.get("type").unwrap();
        assert_eq!(ft, "relationship");
        let rel: Table = tbl.get("relationship").unwrap();
        let col: String = rel.get("collection").unwrap();
        assert_eq!(col, "tags");
        let hm: bool = rel.get("has_many").unwrap();
        assert!(hm);
        let md: i32 = rel.get("max_depth").unwrap();
        assert_eq!(md, 1);
    }

    #[test]
    fn register_schema_get_collection() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        let reg = make_registry_with_collection();
        register_schema(&lua, &crap, reg).unwrap();

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
        let reg = make_registry_with_collection();
        register_schema(&lua, &crap, reg).unwrap();

        let schema: Table = crap.get("schema").unwrap();
        let get_global: mlua::Function = schema.get("get_global").unwrap();
        let result: Value = get_global.call("settings".to_string()).unwrap();
        if let Value::Table(tbl) = result {
            let slug: String = tbl.get("slug").unwrap();
            assert_eq!(slug, "settings");
        } else {
            panic!("Expected Table for global");
        }

        let not_found: Value = get_global.call("nonexistent".to_string()).unwrap();
        assert!(matches!(not_found, Value::Nil));
    }

    #[test]
    fn register_schema_list_collections() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        let reg = make_registry_with_collection();
        register_schema(&lua, &crap, reg).unwrap();

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
        let reg = make_registry_with_collection();
        register_schema(&lua, &crap, reg).unwrap();

        let schema: Table = crap.get("schema").unwrap();
        let list: mlua::Function = schema.get("list_globals").unwrap();
        let result: Table = list.call(()).unwrap();
        let first: Table = result.get(1).unwrap();
        let slug: String = first.get("slug").unwrap();
        assert_eq!(slug, "settings");
    }
}
