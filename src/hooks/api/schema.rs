//! `crap.schema` namespace — read-only schema introspection.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use crate::core::{CollectionDefinition, FieldDefinition, SharedRegistry};

/// Register `crap.schema` — read-only collection/global introspection.
pub(super) fn register_schema(lua: &Lua, crap: &Table, registry: SharedRegistry) -> Result<()> {
    let schema_table = lua.create_table()?;

    let reg = registry.clone();
    let get_collection_fn = lua.create_function(move |lua, slug: String| -> LuaResult<Value> {
        let r = reg
            .read()
            .map_err(|e| RuntimeError(format!("Registry lock: {}", e)))?;

        match r.get_collection(&slug) {
            Some(def) => Ok(Value::Table(collection_def_to_lua_table(lua, def)?)),
            None => Ok(Value::Nil),
        }
    })?;

    schema_table.set("get_collection", get_collection_fn)?;

    let reg = registry.clone();
    let get_global_fn = lua.create_function(move |lua, slug: String| -> LuaResult<Value> {
        let r = reg
            .read()
            .map_err(|e| RuntimeError(format!("Registry lock: {}", e)))?;

        match r.get_global(&slug) {
            Some(def) => {
                let tbl = lua.create_table()?;

                tbl.set("slug", &*def.slug)?;

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
    let list_collections_fn = lua.create_function(move |lua, ()| -> LuaResult<Table> {
        let r = reg
            .read()
            .map_err(|e| RuntimeError(format!("Registry lock: {}", e)))?;

        let tbl = lua.create_table()?;

        let mut i = 0;

        for def in r.collections.values() {
            i += 1;

            let item = lua.create_table()?;

            item.set("slug", &*def.slug)?;

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

    let list_globals_fn = lua.create_function(move |lua, ()| -> LuaResult<Table> {
        let r = reg
            .read()
            .map_err(|e| RuntimeError(format!("Registry lock: {}", e)))?;

        let tbl = lua.create_table()?;

        let mut i = 0;

        for def in r.globals.values() {
            i += 1;

            let item = lua.create_table()?;

            item.set("slug", &*def.slug)?;

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
fn collection_def_to_lua_table(lua: &Lua, def: &CollectionDefinition) -> LuaResult<Table> {
    let tbl = lua.create_table()?;

    tbl.set("slug", &*def.slug)?;

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
fn field_def_to_lua_table(lua: &Lua, f: &FieldDefinition) -> LuaResult<Table> {
    let tbl = lua.create_table()?;

    tbl.set("name", f.name.as_str())?;
    tbl.set("type", f.field_type.as_str())?;
    tbl.set("required", f.required)?;
    tbl.set("localized", f.localized)?;
    tbl.set("unique", f.unique)?;

    if let Some(ref rc) = f.relationship {
        let rel = lua.create_table()?;

        if rc.is_polymorphic() {
            let arr = lua.create_table()?;

            for (i, slug) in rc.polymorphic.iter().enumerate() {
                arr.set(i + 1, &**slug)?;
            }

            rel.set("collection", arr)?;
        } else {
            rel.set("collection", &*rc.collection)?;
        }

        rel.set("has_many", rc.has_many)?;

        if let Some(md) = rc.max_depth {
            rel.set("max_depth", md)?;
        }

        tbl.set("relationship", rel)?;
    }

    if let Some(ml) = f.min_length {
        tbl.set("min_length", ml)?;
    }

    if let Some(ml) = f.max_length {
        tbl.set("max_length", ml)?;
    }

    if let Some(v) = f.min {
        tbl.set("min", v)?;
    }

    if let Some(v) = f.max {
        tbl.set("max", v)?;
    }

    if f.has_many {
        tbl.set("has_many", true)?;
    }

    if let Some(ref md) = f.min_date {
        tbl.set("min_date", md.as_str())?;
    }

    if let Some(ref md) = f.max_date {
        tbl.set("max_date", md.as_str())?;
    }

    if let Some(ref lang) = f.admin.language {
        tbl.set("language", lang.as_str())?;
    }

    if !f.admin.features.is_empty() {
        let features = lua.create_table()?;

        for (i, feat) in f.admin.features.iter().enumerate() {
            features.set(i + 1, feat.as_str())?;
        }

        tbl.set("features", features)?;
    }

    if let Some(ref p) = f.admin.picker {
        tbl.set("picker", p.as_str())?;
    }

    if let Some(ref jc) = f.join {
        tbl.set("collection", &*jc.collection)?;
        tbl.set("on", jc.on.as_str())?;
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

            if let Some(ref g) = b.group {
                bt.set("group", g.as_str())?;
            }

            if let Some(ref url) = b.image_url {
                bt.set("image_url", url.as_str())?;
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
    use crate::core::{
        CollectionDefinition, Registry, SharedRegistry,
        collection::GlobalDefinition,
        field::{
            BlockDefinition, FieldDefinition, FieldType, JoinConfig, LocalizedString,
            RelationshipConfig,
        },
    };
    use mlua::Lua;
    use std::sync::{Arc, RwLock};

    fn make_registry_with_collection() -> SharedRegistry {
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

    #[test]
    fn field_def_to_lua_table_polymorphic_relationship() {
        let lua = Lua::new();
        let mut rel_cfg = RelationshipConfig::new("articles", true);
        rel_cfg.polymorphic = vec!["articles".into(), "pages".into()];
        let field = FieldDefinition::builder("refs", FieldType::Relationship)
            .relationship(rel_cfg)
            .build();
        let tbl = field_def_to_lua_table(&lua, &field).unwrap();

        let rel: Table = tbl.get("relationship").unwrap();
        // Polymorphic: collection should be a table (array), not a string
        let col: Table = rel.get("collection").unwrap();
        let first: String = col.get(1).unwrap();
        assert_eq!(first, "articles");
        let second: String = col.get(2).unwrap();
        assert_eq!(second, "pages");
        let hm: bool = rel.get("has_many").unwrap();
        assert!(hm);
    }

    #[test]
    fn field_def_to_lua_table_non_polymorphic_relationship() {
        let lua = Lua::new();
        let field = FieldDefinition::builder("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new("users", false))
            .build();
        let tbl = field_def_to_lua_table(&lua, &field).unwrap();

        let rel: Table = tbl.get("relationship").unwrap();
        // Non-polymorphic: collection should be a string
        let col: String = rel.get("collection").unwrap();
        assert_eq!(col, "users");
    }

    #[test]
    fn field_def_to_lua_table_richtext_features() {
        let lua = Lua::new();
        let mut field = FieldDefinition::builder("body", FieldType::Richtext).build();
        field.admin.features = vec![
            "bold".to_string(),
            "italic".to_string(),
            "heading".to_string(),
        ];
        let tbl = field_def_to_lua_table(&lua, &field).unwrap();

        let features: Table = tbl.get("features").unwrap();
        let f1: String = features.get(1).unwrap();
        assert_eq!(f1, "bold");
        let f2: String = features.get(2).unwrap();
        assert_eq!(f2, "italic");
        let f3: String = features.get(3).unwrap();
        assert_eq!(f3, "heading");
    }

    #[test]
    fn field_def_to_lua_table_richtext_no_features() {
        let lua = Lua::new();
        let field = FieldDefinition::builder("body", FieldType::Richtext).build();
        let tbl = field_def_to_lua_table(&lua, &field).unwrap();

        // No features key when empty
        let features: mlua::Result<Table> = tbl.get("features");
        assert!(features.is_err() || matches!(tbl.get::<Value>("features"), Ok(Value::Nil)));
    }

    // Covers min_length, max_length, min, max in field_def_to_lua_table
    #[test]
    fn field_def_to_lua_table_min_max_length_and_value() {
        let lua = Lua::new();
        let field = FieldDefinition::builder("score", FieldType::Number)
            .min_length(2)
            .max_length(100)
            .min(0.5)
            .max(99.9)
            .build();
        let tbl = field_def_to_lua_table(&lua, &field).unwrap();

        let min_length: usize = tbl.get("min_length").unwrap();
        assert_eq!(min_length, 2);
        let max_length: usize = tbl.get("max_length").unwrap();
        assert_eq!(max_length, 100);
        let min: f64 = tbl.get("min").unwrap();
        assert!((min - 0.5).abs() < f64::EPSILON);
        let max: f64 = tbl.get("max").unwrap();
        assert!((max - 99.9).abs() < 1e-9);
    }

    // Covers has_many = true in field_def_to_lua_table (lines 174-176)
    #[test]
    fn field_def_to_lua_table_has_many() {
        let lua = Lua::new();
        let field = FieldDefinition::builder("tags", FieldType::Select)
            .has_many(true)
            .build();
        let tbl = field_def_to_lua_table(&lua, &field).unwrap();

        let has_many: bool = tbl.get("has_many").unwrap();
        assert!(has_many);
    }

    // Covers min_date and max_date in field_def_to_lua_table (lines 178-183)
    #[test]
    fn field_def_to_lua_table_min_max_date() {
        let lua = Lua::new();
        let field = FieldDefinition::builder("published_at", FieldType::Date)
            .min_date("2020-01-01")
            .max_date("2030-12-31")
            .build();
        let tbl = field_def_to_lua_table(&lua, &field).unwrap();

        let min_date: String = tbl.get("min_date").unwrap();
        assert_eq!(min_date, "2020-01-01");
        let max_date: String = tbl.get("max_date").unwrap();
        assert_eq!(max_date, "2030-12-31");
    }

    // Covers admin.language in field_def_to_lua_table (lines 185-187)
    #[test]
    fn field_def_to_lua_table_language() {
        let lua = Lua::new();
        let mut field = FieldDefinition::builder("snippet", FieldType::Code).build();
        field.admin.language = Some("javascript".to_string());
        let tbl = field_def_to_lua_table(&lua, &field).unwrap();

        let lang: String = tbl.get("language").unwrap();
        assert_eq!(lang, "javascript");
    }

    // Covers admin.picker in field_def_to_lua_table (lines 197-199)
    #[test]
    fn field_def_to_lua_table_picker() {
        let lua = Lua::new();
        let mut field = FieldDefinition::builder("layout", FieldType::Blocks).build();
        field.admin.picker = Some("card".to_string());
        let tbl = field_def_to_lua_table(&lua, &field).unwrap();

        let picker: String = tbl.get("picker").unwrap();
        assert_eq!(picker, "card");
    }

    // Covers join config in field_def_to_lua_table (lines 201-204)
    #[test]
    fn field_def_to_lua_table_join_config() {
        let lua = Lua::new();
        let field = FieldDefinition::builder("comments", FieldType::Join)
            .join(JoinConfig::new("comments", "post_id"))
            .build();
        let tbl = field_def_to_lua_table(&lua, &field).unwrap();

        let collection: String = tbl.get("collection").unwrap();
        assert_eq!(collection, "comments");
        let on: String = tbl.get("on").unwrap();
        assert_eq!(on, "post_id");
    }

    // Covers sub-fields in field_def_to_lua_table (lines 218-224) — array/group type
    #[test]
    fn field_def_to_lua_table_sub_fields() {
        let lua = Lua::new();
        let field = FieldDefinition::builder("address", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("street", FieldType::Text).build(),
                FieldDefinition::builder("city", FieldType::Text)
                    .required(true)
                    .build(),
            ])
            .build();
        let tbl = field_def_to_lua_table(&lua, &field).unwrap();

        let sub: Table = tbl.get("fields").unwrap();
        let sf1: Table = sub.get(1).unwrap();
        let name1: String = sf1.get("name").unwrap();
        assert_eq!(name1, "street");
        let sf2: Table = sub.get(2).unwrap();
        let name2: String = sf2.get("name").unwrap();
        assert_eq!(name2, "city");
        let req2: bool = sf2.get("required").unwrap();
        assert!(req2);
    }

    // Covers get_global when plural label is absent (the `if let Some` branch for plural
    // evaluates to None and skips — exercises the None path rather than Some).
    #[test]
    fn get_global_with_no_plural_label() {
        let mut reg = Registry::new();
        let mut branding = GlobalDefinition::new("branding");
        branding.labels.singular = Some(LocalizedString::Plain("Brand".to_string()));
        // no plural label
        reg.register_global(branding);
        let registry = Arc::new(RwLock::new(reg));

        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_schema(&lua, &crap, registry).unwrap();

        let schema: Table = crap.get("schema").unwrap();
        let get_global: mlua::Function = schema.get("get_global").unwrap();
        let result: Value = get_global.call("branding".to_string()).unwrap();

        if let Value::Table(tbl) = result {
            let slug: String = tbl.get("slug").unwrap();
            assert_eq!(slug, "branding");
            let labels: Table = tbl.get("labels").unwrap();
            let singular: String = labels.get("singular").unwrap();
            assert_eq!(singular, "Brand");
            // plural should be nil since we didn't set it
            let plural: Value = labels.get("plural").unwrap();
            assert!(matches!(plural, Value::Nil));
        } else {
            panic!("Expected Table for global");
        }
    }

    #[test]
    fn field_def_to_lua_table_blocks_with_group_and_image() {
        let lua = Lua::new();
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
        let tbl = field_def_to_lua_table(&lua, &field).unwrap();
        let blocks: Table = tbl.get("blocks").unwrap();

        let b1: Table = blocks.get(1).unwrap();
        assert_eq!(b1.get::<String>("type").unwrap(), "hero");
        assert_eq!(b1.get::<String>("group").unwrap(), "Layout");
        assert_eq!(
            b1.get::<String>("image_url").unwrap(),
            "/static/blocks/hero.svg"
        );

        let b2: Table = blocks.get(2).unwrap();
        assert_eq!(b2.get::<String>("group").unwrap(), "Content");
        assert!(matches!(b2.get::<Value>("image_url"), Ok(Value::Nil)));

        let b3: Table = blocks.get(3).unwrap();
        assert!(matches!(b3.get::<Value>("group"), Ok(Value::Nil)));
        assert!(matches!(b3.get::<Value>("image_url"), Ok(Value::Nil)));
    }
}
