//! Lua table serializers for FieldDefinition.
//! Produces round-trip compatible tables that can be passed back to parse_fields().

use mlua::{Lua, Result as LuaResult, Table};
use serde_json::Value as JsonValue;

use crate::core::{
    FieldDefinition,
    field::{FieldAccess, FieldHooks},
};

use super::{admin::field_admin_to_lua, helpers::localized_string_to_lua};

/// Convert a FieldDefinition to a full Lua table compatible with parse_fields().
pub(super) fn field_config_to_lua(lua: &Lua, f: &FieldDefinition) -> LuaResult<Table> {
    let tbl = lua.create_table()?;

    tbl.set("name", f.name.as_str())?;
    tbl.set("type", f.field_type.as_str())?;

    if f.required {
        tbl.set("required", true)?;
    }

    if f.unique {
        tbl.set("unique", true)?;
    }

    if f.localized {
        tbl.set("localized", true)?;
    }

    if let Some(ref v) = f.validate {
        tbl.set("validate", v.as_str())?;
    }

    if let Some(ref dv) = f.default_value {
        match dv {
            JsonValue::Bool(b) => {
                tbl.set("default_value", *b)?;
            }
            JsonValue::Number(n) => {
                if let Some(i) = n.as_i64() {
                    tbl.set("default_value", i)?;
                } else if let Some(f_val) = n.as_f64() {
                    tbl.set("default_value", f_val)?;
                }
            }
            JsonValue::String(s) => {
                tbl.set("default_value", s.as_str())?;
            }
            _ => {}
        }
    }

    if let Some(ref pa) = f.picker_appearance {
        tbl.set("picker_appearance", pa.as_str())?;
    }

    if f.timezone {
        tbl.set("timezone", true)?;
    }

    if let Some(ref dtz) = f.default_timezone {
        tbl.set("default_timezone", dtz.as_str())?;
    }

    // has_many for scalar fields (text, number, select — not relationship/upload which use RelationshipConfig)
    if f.has_many && f.relationship.is_none() {
        tbl.set("has_many", true)?;
    }

    // options (select fields)
    if !f.options.is_empty() {
        let opts = lua.create_table()?;

        for (i, opt) in f.options.iter().enumerate() {
            let o = lua.create_table()?;
            o.set("label", localized_string_to_lua(lua, &opt.label)?)?;
            o.set("value", opt.value.as_str())?;
            opts.set(i + 1, o)?;
        }

        tbl.set("options", opts)?;
    }

    // admin
    if let Some(admin) = field_admin_to_lua(lua, &f.admin)? {
        tbl.set("admin", admin)?;
    }

    // hooks
    if let Some(hooks) = field_hooks_to_lua(lua, &f.hooks)? {
        tbl.set("hooks", hooks)?;
    }

    // access
    if let Some(access) = field_access_to_lua(lua, &f.access)? {
        tbl.set("access", access)?;
    }

    // mcp
    if let Some(ref desc) = f.mcp.description {
        let mcp = lua.create_table()?;

        mcp.set("description", desc.as_str())?;

        tbl.set("mcp", mcp)?;
    }

    // relationship
    if let Some(ref rc) = f.relationship {
        let rel = lua.create_table()?;

        rel.set("collection", &*rc.collection)?;

        if rc.has_many {
            rel.set("has_many", true)?;
        }

        if let Some(md) = rc.max_depth {
            rel.set("max_depth", md)?;
        }

        tbl.set("relationship", rel)?;
    }

    // sub-fields (array, group)
    if !f.fields.is_empty() {
        let sub = lua.create_table()?;

        for (i, sf) in f.fields.iter().enumerate() {
            sub.set(i + 1, field_config_to_lua(lua, sf)?)?;
        }

        tbl.set("fields", sub)?;
    }

    // blocks
    if !f.blocks.is_empty() {
        let blocks = lua.create_table()?;

        for (i, b) in f.blocks.iter().enumerate() {
            let bt = lua.create_table()?;

            bt.set("type", b.block_type.as_str())?;

            if let Some(ref lbl) = b.label {
                bt.set("label", localized_string_to_lua(lua, lbl)?)?;
            }

            if let Some(ref g) = b.group {
                bt.set("group", g.as_str())?;
            }

            if let Some(ref url) = b.image_url {
                bt.set("image_url", url.as_str())?;
            }

            let bf = lua.create_table()?;

            for (j, sf) in b.fields.iter().enumerate() {
                bf.set(j + 1, field_config_to_lua(lua, sf)?)?;
            }

            bt.set("fields", bf)?;

            blocks.set(i + 1, bt)?;
        }

        tbl.set("blocks", blocks)?;
    }

    // tabs (for Tabs field type)
    if !f.tabs.is_empty() {
        let tabs = lua.create_table()?;

        for (i, tab) in f.tabs.iter().enumerate() {
            let tt = lua.create_table()?;

            tt.set("label", tab.label.as_str())?;

            if let Some(ref desc) = tab.description {
                tt.set("description", desc.as_str())?;
            }

            let tf = lua.create_table()?;

            for (j, sf) in tab.fields.iter().enumerate() {
                tf.set(j + 1, field_config_to_lua(lua, sf)?)?;
            }

            tt.set("fields", tf)?;

            tabs.set(i + 1, tt)?;
        }

        tbl.set("tabs", tabs)?;
    }

    Ok(tbl)
}

/// Convert a `FieldHooks` to a Lua table. Returns `None` if no hooks are set.
fn field_hooks_to_lua(lua: &Lua, hooks: &FieldHooks) -> LuaResult<Option<Table>> {
    let pairs: &[(&str, &[String])] = &[
        ("before_validate", &hooks.before_validate),
        ("before_change", &hooks.before_change),
        ("after_change", &hooks.after_change),
        ("after_read", &hooks.after_read),
    ];

    if pairs.iter().all(|(_, list)| list.is_empty()) {
        return Ok(None);
    }

    let tbl = lua.create_table()?;

    for (key, list) in pairs {
        if !list.is_empty() {
            let arr = lua.create_table()?;

            for (i, s) in list.iter().enumerate() {
                arr.set(i + 1, s.as_str())?;
            }

            tbl.set(*key, arr)?;
        }
    }

    Ok(Some(tbl))
}

/// Convert a `FieldAccess` to a Lua table. Returns `None` if no access rules are set.
fn field_access_to_lua(lua: &Lua, access: &FieldAccess) -> LuaResult<Option<Table>> {
    if access.read.is_none() && access.create.is_none() && access.update.is_none() {
        return Ok(None);
    }

    let tbl = lua.create_table()?;

    if let Some(ref s) = access.read {
        tbl.set("read", s.as_str())?;
    }

    if let Some(ref s) = access.create {
        tbl.set("create", s.as_str())?;
    }

    if let Some(ref s) = access.update {
        tbl.set("update", s.as_str())?;
    }

    Ok(Some(tbl))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{self, Value};
    use serde_json::json;

    use crate::core::field::{
        BlockDefinition, FieldAdmin, FieldTab, FieldType, LocalizedString, McpFieldConfig,
        RelationshipConfig, SelectOption,
    };

    #[test]
    fn test_field_config_to_lua_simple() {
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .unique(true)
            .validate("hooks.validate.title_check")
            .default_value(json!("untitled"))
            .build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        assert_eq!(tbl.get::<String>("name").unwrap(), "title");
        assert_eq!(tbl.get::<String>("type").unwrap(), "text");
        assert!(tbl.get::<bool>("required").unwrap());
        assert!(tbl.get::<bool>("unique").unwrap());
        assert_eq!(
            tbl.get::<String>("validate").unwrap(),
            "hooks.validate.title_check"
        );
        assert_eq!(tbl.get::<String>("default_value").unwrap(), "untitled");
    }

    #[test]
    fn test_field_config_to_lua_with_relationship() {
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("author", FieldType::Relationship)
            .relationship({
                let mut rc = RelationshipConfig::new("users", true);
                rc.max_depth = Some(2);
                rc
            })
            .build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        let rel: mlua::Table = tbl.get("relationship").unwrap();
        assert_eq!(rel.get::<String>("collection").unwrap(), "users");
        assert!(rel.get::<bool>("has_many").unwrap());
        assert_eq!(rel.get::<i32>("max_depth").unwrap(), 2);
    }

    #[test]
    fn test_field_config_to_lua_with_options() {
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("status", FieldType::Select)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Draft".to_string()), "draft"),
                SelectOption::new(LocalizedString::Plain("Published".to_string()), "published"),
            ])
            .build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        let opts: mlua::Table = tbl.get("options").unwrap();
        let o1: mlua::Table = opts.get(1).unwrap();
        assert_eq!(o1.get::<String>("value").unwrap(), "draft");
    }

    #[test]
    fn test_field_config_to_lua_with_blocks() {
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![{
                let mut b = BlockDefinition::new(
                    "text",
                    vec![FieldDefinition::builder("body", FieldType::Text).build()],
                );
                b.label = Some(LocalizedString::Plain("Text Block".to_string()));
                b
            }])
            .build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        let blocks: mlua::Table = tbl.get("blocks").unwrap();
        let b1: mlua::Table = blocks.get(1).unwrap();
        assert_eq!(b1.get::<String>("type").unwrap(), "text");
        assert_eq!(b1.get::<String>("label").unwrap(), "Text Block");
        let bf: mlua::Table = b1.get("fields").unwrap();
        let bf1: mlua::Table = bf.get(1).unwrap();
        assert_eq!(bf1.get::<String>("name").unwrap(), "body");
    }

    #[test]
    fn test_field_config_to_lua_has_many_text() {
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("tags", FieldType::Text)
            .has_many(true)
            .build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        assert!(tbl.get::<bool>("has_many").unwrap());
    }

    #[test]
    fn test_field_config_to_lua_blocks_group_and_image() {
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![{
                let mut b = BlockDefinition::new("hero", vec![]);
                b.group = Some("Layout".to_string());
                b.image_url = Some("/static/hero.svg".to_string());
                b
            }])
            .build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        let blocks: mlua::Table = tbl.get("blocks").unwrap();
        let b1: mlua::Table = blocks.get(1).unwrap();
        assert_eq!(b1.get::<String>("group").unwrap(), "Layout");
        assert_eq!(b1.get::<String>("image_url").unwrap(), "/static/hero.svg");
    }

    #[test]
    fn test_field_config_to_lua_with_admin_and_hooks() {
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("title", FieldType::Text)
            .admin(
                FieldAdmin::builder()
                    .label(LocalizedString::Plain("Title".to_string()))
                    .placeholder(LocalizedString::Plain("Enter title".to_string()))
                    .description(LocalizedString::Plain("The document title".to_string()))
                    .hidden(true)
                    .readonly(true)
                    .width("50%")
                    .collapsed(false)
                    .build(),
            )
            .hooks(FieldHooks {
                before_validate: vec!["hooks.field.trim".to_string()],
                before_change: vec!["hooks.field.upper".to_string()],
                after_change: Vec::new(),
                after_read: vec!["hooks.field.format".to_string()],
            })
            .access(FieldAccess {
                read: Some("hooks.access.check".to_string()),
                create: Some("hooks.access.admin".to_string()),
                update: None,
            })
            .build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();

        let admin: mlua::Table = tbl.get("admin").unwrap();
        assert_eq!(admin.get::<String>("label").unwrap(), "Title");
        assert!(admin.get::<bool>("hidden").unwrap());
        assert!(admin.get::<bool>("readonly").unwrap());
        assert_eq!(admin.get::<String>("width").unwrap(), "50%");
        assert!(!admin.get::<bool>("collapsed").unwrap());

        let hooks: mlua::Table = tbl.get("hooks").unwrap();
        let bv: mlua::Table = hooks.get("before_validate").unwrap();
        assert_eq!(bv.get::<String>(1).unwrap(), "hooks.field.trim");
        let bc: mlua::Table = hooks.get("before_change").unwrap();
        assert_eq!(bc.get::<String>(1).unwrap(), "hooks.field.upper");
        let ar: mlua::Table = hooks.get("after_read").unwrap();
        assert_eq!(ar.get::<String>(1).unwrap(), "hooks.field.format");

        let access: mlua::Table = tbl.get("access").unwrap();
        assert_eq!(access.get::<String>("read").unwrap(), "hooks.access.check");
        assert_eq!(
            access.get::<String>("create").unwrap(),
            "hooks.access.admin"
        );
    }

    /// Regression test: field_config_to_lua must emit ALL FieldAdmin properties
    /// so that plugins using config.list() + define() don't lose admin settings.
    #[test]
    fn test_field_config_to_lua_admin_roundtrip_all_properties() {
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("content", FieldType::Blocks)
            .admin(
                FieldAdmin::builder()
                    .label(LocalizedString::Plain("Content".to_string()))
                    .placeholder(LocalizedString::Plain("Add content...".to_string()))
                    .description(LocalizedString::Plain("Main content area".to_string()))
                    .width("full")
                    .collapsed(false)
                    .label_field("heading")
                    .row_label("hooks.content_row_label")
                    .labels_singular(LocalizedString::Plain("Block".to_string()))
                    .labels_plural(LocalizedString::Plain("Blocks".to_string()))
                    .position("main")
                    .condition("hooks.show_content")
                    .step("1")
                    .rows(12)
                    .language("json")
                    .features(vec!["bold".to_string(), "italic".to_string()])
                    .picker("card")
                    .richtext_format("json")
                    .nodes(vec!["cta".to_string()])
                    .build(),
            )
            .build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        let admin: mlua::Table = tbl.get("admin").unwrap();

        // Every FieldAdmin property must be present
        assert_eq!(admin.get::<String>("label").unwrap(), "Content");
        assert_eq!(
            admin.get::<String>("placeholder").unwrap(),
            "Add content..."
        );
        assert_eq!(
            admin.get::<String>("description").unwrap(),
            "Main content area"
        );
        assert_eq!(admin.get::<String>("width").unwrap(), "full");
        assert!(!admin.get::<bool>("collapsed").unwrap());
        assert_eq!(admin.get::<String>("label_field").unwrap(), "heading");
        assert_eq!(
            admin.get::<String>("row_label").unwrap(),
            "hooks.content_row_label"
        );
        let labels: mlua::Table = admin.get("labels").unwrap();
        assert_eq!(labels.get::<String>("singular").unwrap(), "Block");
        assert_eq!(labels.get::<String>("plural").unwrap(), "Blocks");
        assert_eq!(admin.get::<String>("position").unwrap(), "main");
        assert_eq!(
            admin.get::<String>("condition").unwrap(),
            "hooks.show_content"
        );
        assert_eq!(admin.get::<String>("step").unwrap(), "1");
        assert_eq!(admin.get::<u32>("rows").unwrap(), 12);
        assert_eq!(admin.get::<String>("language").unwrap(), "json");
        let features: mlua::Table = admin.get("features").unwrap();
        assert_eq!(features.get::<String>(1).unwrap(), "bold");
        assert_eq!(features.get::<String>(2).unwrap(), "italic");
        assert_eq!(admin.get::<String>("picker").unwrap(), "card");
        assert_eq!(admin.get::<String>("format").unwrap(), "json");
        let nodes: mlua::Table = admin.get("nodes").unwrap();
        assert_eq!(nodes.get::<String>(1).unwrap(), "cta");
    }

    #[test]
    fn test_field_config_to_lua_default_values() {
        let lua = mlua::Lua::new();

        // Bool default
        let f_bool = FieldDefinition::builder("active", FieldType::Text)
            .default_value(json!(true))
            .build();
        let tbl = field_config_to_lua(&lua, &f_bool).unwrap();
        assert!(tbl.get::<bool>("default_value").unwrap());

        // Integer default
        let f_int = FieldDefinition::builder("count", FieldType::Text)
            .default_value(json!(42))
            .build();
        let tbl = field_config_to_lua(&lua, &f_int).unwrap();
        assert_eq!(tbl.get::<i64>("default_value").unwrap(), 42);

        // Float default
        let f_float = FieldDefinition::builder("price", FieldType::Text)
            .default_value(json!(3.15))
            .build();
        let tbl = field_config_to_lua(&lua, &f_float).unwrap();
        let val: f64 = tbl.get("default_value").unwrap();
        assert!((val - 3.15).abs() < f64::EPSILON);
    }

    #[test]
    fn test_field_config_to_lua_with_tabs() {
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("content", FieldType::Tabs)
            .tabs(vec![
                {
                    let mut t = FieldTab::new(
                        "General",
                        vec![FieldDefinition::builder("title", FieldType::Text).build()],
                    );
                    t.description = Some("General settings".to_string());
                    t
                },
                FieldTab::new("Advanced", vec![]),
            ])
            .build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        let tabs: mlua::Table = tbl.get("tabs").unwrap();
        assert_eq!(tabs.raw_len(), 2);
        let t1: mlua::Table = tabs.get(1).unwrap();
        assert_eq!(t1.get::<String>("label").unwrap(), "General");
        assert_eq!(t1.get::<String>("description").unwrap(), "General settings");
        let tf: mlua::Table = t1.get("fields").unwrap();
        assert_eq!(tf.raw_len(), 1);
        let t2: mlua::Table = tabs.get(2).unwrap();
        assert_eq!(t2.get::<String>("label").unwrap(), "Advanced");
        // description absent when None
        let desc_val: Value = t2.get("description").unwrap();
        assert!(matches!(desc_val, Value::Nil));
    }

    #[test]
    fn test_field_config_to_lua_with_sub_fields() {
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("address", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("street", FieldType::Text).build(),
                FieldDefinition::builder("city", FieldType::Text).build(),
            ])
            .build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        let sub: mlua::Table = tbl.get("fields").unwrap();
        assert_eq!(sub.raw_len(), 2);
        let sf1: mlua::Table = sub.get(1).unwrap();
        assert_eq!(sf1.get::<String>("name").unwrap(), "street");
        let sf2: mlua::Table = sub.get(2).unwrap();
        assert_eq!(sf2.get::<String>("name").unwrap(), "city");
    }

    #[test]
    fn test_field_config_to_lua_mcp_description() {
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("title", FieldType::Text)
            .mcp(McpFieldConfig {
                description: Some("The post title".to_string()),
            })
            .build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        let mcp: mlua::Table = tbl.get("mcp").unwrap();
        assert_eq!(mcp.get::<String>("description").unwrap(), "The post title");
    }

    #[test]
    fn test_field_config_to_lua_localized_and_picker_appearance() {
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("body", FieldType::Text)
            .localized(true)
            .picker_appearance("drawer")
            .build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        assert!(tbl.get::<bool>("localized").unwrap());
        assert_eq!(tbl.get::<String>("picker_appearance").unwrap(), "drawer");
    }

    #[test]
    fn test_field_config_to_lua_timezone_roundtrip() {
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("start_date", FieldType::Date)
            .timezone(true)
            .default_timezone("America/New_York")
            .picker_appearance("dayAndTime")
            .build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();

        assert!(tbl.get::<bool>("timezone").unwrap());
        assert_eq!(
            tbl.get::<String>("default_timezone").unwrap(),
            "America/New_York"
        );

        // Verify it survives re-parse (simulates plugin round-trip)
        let fields_tbl = lua.create_table().unwrap();
        fields_tbl.set(1, tbl).unwrap();
        let parsed = crate::hooks::api::parse::fields::parse_fields(&fields_tbl).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(
            parsed[0].timezone,
            "timezone must survive serialization round-trip"
        );
        assert_eq!(
            parsed[0].default_timezone.as_deref(),
            Some("America/New_York")
        );
    }

    #[test]
    fn test_field_config_to_lua_no_timezone_omitted() {
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("created_at", FieldType::Date).build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();

        // timezone should not be present when false
        assert!(tbl.get::<Option<bool>>("timezone").unwrap().is_none());
        assert!(
            tbl.get::<Option<String>>("default_timezone")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_field_config_to_lua_admin_labels_plural_only() {
        // When labels_singular is None but labels_plural is Some, the else branch runs
        let lua = mlua::Lua::new();
        let f = FieldDefinition::builder("items", FieldType::Text)
            .admin(
                FieldAdmin::builder()
                    .labels_plural(LocalizedString::Plain("Items".to_string()))
                    .build(),
            )
            .build();
        let tbl = field_config_to_lua(&lua, &f).unwrap();
        let admin: mlua::Table = tbl.get("admin").unwrap();
        let labels: mlua::Table = admin.get("labels").unwrap();
        assert_eq!(labels.get::<String>("plural").unwrap(), "Items");
        // singular should not be present
        let singular_val: Value = labels.get("singular").unwrap();
        assert!(matches!(singular_val, Value::Nil));
    }
}
