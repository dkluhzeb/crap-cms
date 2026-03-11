//! Register `crap.fields` — per-type factory functions that set `type` and return the table.
//! `crap.fields.text({ name = "title" })` is equivalent to `{ type = "text", name = "title" }`.

use anyhow::Result;
use mlua::{Lua, Table};

pub(super) fn register_fields(lua: &Lua, crap: &Table) -> Result<()> {
    let fields_table = lua.create_table()?;

    let field_types = [
        "text",
        "number",
        "textarea",
        "richtext",
        "select",
        "radio",
        "checkbox",
        "date",
        "email",
        "json",
        "code",
        "relationship",
        "upload",
        "array",
        "group",
        "blocks",
        "row",
        "collapsible",
        "tabs",
        "join",
    ];

    for type_name in field_types {
        let type_str = type_name.to_string();
        let factory = lua.create_function(move |_lua, config: Table| {
            config.set("type", type_str.clone())?;
            Ok(config)
        })?;
        fields_table.set(type_name, factory)?;
    }

    crap.set("fields", fields_table)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{Function, Lua, Table};

    #[test]
    fn test_fields_factory_sets_type() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_fields(&lua, &crap).unwrap();
        let fields: Table = crap.get("fields").unwrap();

        let factory: Function = fields.get("text").unwrap();
        let config = lua.create_table().unwrap();
        config.set("name", "title").unwrap();
        config.set("required", true).unwrap();
        let result: Table = factory.call(config).unwrap();
        assert_eq!(result.get::<String>("type").unwrap(), "text");
        assert_eq!(result.get::<String>("name").unwrap(), "title");
        assert_eq!(result.get::<bool>("required").unwrap(), true);
    }

    #[test]
    fn test_fields_factory_all_types_registered() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_fields(&lua, &crap).unwrap();
        let fields: Table = crap.get("fields").unwrap();

        let expected = [
            "text",
            "number",
            "textarea",
            "richtext",
            "select",
            "radio",
            "checkbox",
            "date",
            "email",
            "json",
            "code",
            "relationship",
            "upload",
            "array",
            "group",
            "blocks",
            "row",
            "collapsible",
            "tabs",
            "join",
        ];
        for type_name in expected {
            let factory: Function = fields.get(type_name).unwrap();
            let config = lua.create_table().unwrap();
            config.set("name", "test").unwrap();
            let result: Table = factory.call(config).unwrap();
            assert_eq!(
                result.get::<String>("type").unwrap(),
                type_name,
                "Factory for '{}' should set type correctly",
                type_name
            );
        }
    }

    #[test]
    fn test_fields_factory_compatible_with_parse() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_fields(&lua, &crap).unwrap();
        let fields: Table = crap.get("fields").unwrap();

        let factory: Function = fields.get("select").unwrap();
        let config = lua.create_table().unwrap();
        config.set("name", "status").unwrap();
        let opts = lua.create_table().unwrap();
        let opt1 = lua.create_table().unwrap();
        opt1.set("label", "Draft").unwrap();
        opt1.set("value", "draft").unwrap();
        opts.set(1, opt1).unwrap();
        config.set("options", opts).unwrap();
        let result: Table = factory.call(config).unwrap();

        let fields_arr = lua.create_table().unwrap();
        fields_arr.set(1, result).unwrap();
        let parsed = super::super::parse::fields::parse_fields(&fields_arr).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "status");
        assert_eq!(parsed[0].field_type, crate::core::field::FieldType::Select);
        assert_eq!(parsed[0].options.len(), 1);
        assert_eq!(parsed[0].options[0].value, "draft");
    }
}
