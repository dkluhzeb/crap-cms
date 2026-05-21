//! Register `crap.fields` — per-type factory functions that set `type` and return the table.
//! `crap.fields.text({ name = "title" })` is equivalent to `{ type = "text", name = "title" }`.

use anyhow::Result;
use mlua::{Lua, Result as LuaResult, Table};

use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Define a `crap.fields.{name}({ ... })` factory function.
///
/// Each factory just stamps `type = "<lua_type>"` onto the supplied
/// table and returns it — equivalent to `{ type = "<lua_type>", ...rest }`.
/// The macro keeps the 20 declarations free of copy-paste boilerplate
/// while preserving per-type doc strings and typed config classes for
/// the Lua-side autocomplete.
macro_rules! field_factory {
    ($fn_name:ident, $lua_type:literal, $path:literal, $cfg_class:literal, $doc:literal) => {
        #[doc = $doc]
        #[lua_fn(path = $path, returns = "crap.FieldDefinition")]
        fn $fn_name(_: &Lua, #[lua(ty = $cfg_class)] config: Table) -> LuaResult<Table> {
            config.set("type", $lua_type)?;
            Ok(config)
        }
    };
}

field_factory!(
    field_text,
    "text",
    "crap.fields.text",
    "crap.TextField",
    "Create a text field (single-line string)."
);
field_factory!(
    field_number,
    "number",
    "crap.fields.number",
    "crap.NumberField",
    "Create a number field (integer or float)."
);
field_factory!(
    field_textarea,
    "textarea",
    "crap.fields.textarea",
    "crap.TextareaField",
    "Create a textarea field (multi-line text)."
);
field_factory!(
    field_richtext,
    "richtext",
    "crap.fields.richtext",
    "crap.RichtextField",
    "Create a richtext field (`ProseMirror` editor, stored as HTML by default or JSON)."
);
field_factory!(
    field_select,
    "select",
    "crap.fields.select",
    "crap.SelectField",
    "Create a select field (dropdown, single or multi)."
);
field_factory!(
    field_radio,
    "radio",
    "crap.fields.radio",
    "crap.RadioField",
    "Create a radio field (radio button group)."
);
field_factory!(
    field_checkbox,
    "checkbox",
    "crap.fields.checkbox",
    "crap.CheckboxField",
    "Create a checkbox field (boolean true/false)."
);
field_factory!(
    field_date,
    "date",
    "crap.fields.date",
    "crap.DateField",
    "Create a date field (ISO 8601 date/datetime)."
);
field_factory!(
    field_email,
    "email",
    "crap.fields.email",
    "crap.EmailField",
    "Create an email field (validated email address)."
);
field_factory!(
    field_json,
    "json",
    "crap.fields.json",
    "crap.JsonField",
    "Create a JSON field (arbitrary JSON blob)."
);
field_factory!(
    field_code,
    "code",
    "crap.fields.code",
    "crap.CodeField",
    "Create a code field (`CodeMirror` editor). Set `admin.language` for syntax mode."
);
field_factory!(
    field_relationship,
    "relationship",
    "crap.fields.relationship",
    "crap.RelationshipField",
    "Create a relationship field (reference to another collection)."
);
field_factory!(
    field_upload,
    "upload",
    "crap.fields.upload",
    "crap.UploadField",
    "Create an upload field (file upload, references an upload collection)."
);
field_factory!(
    field_array,
    "array",
    "crap.fields.array",
    "crap.ArrayField",
    "Create an array field (repeatable sub-fields)."
);
field_factory!(
    field_group,
    "group",
    "crap.fields.group",
    "crap.GroupField",
    "Create a group field (visual grouping with column prefix)."
);
field_factory!(
    field_blocks,
    "blocks",
    "crap.fields.blocks",
    "crap.BlocksField",
    "Create a blocks field (flexible content blocks)."
);
field_factory!(
    field_row,
    "row",
    "crap.fields.row",
    "crap.RowField",
    "Create a row field (layout-only horizontal grouping, no column prefix)."
);
field_factory!(
    field_collapsible,
    "collapsible",
    "crap.fields.collapsible",
    "crap.CollapsibleField",
    "Create a collapsible field (layout-only collapsible section, no column prefix)."
);
field_factory!(
    field_tabs,
    "tabs",
    "crap.fields.tabs",
    "crap.TabsField",
    "Create a tabs field (layout-only tabbed container, no column prefix)."
);
field_factory!(
    field_join,
    "join",
    "crap.fields.join",
    "crap.JoinField",
    "Create a join field (virtual reverse-relationship, read-only)."
);

lua_table! {
    name: crap_fields,
    path: "crap.fields",
    state: (),
    header: "Field factory functions. Each sets `type` automatically and returns a\n`crap.FieldDefinition`-compatible table. Use these instead of raw tables\nfor precise per-type autocomplete.\n\nExample:\n```lua\ncrap.collections.define(\"posts\", {\n    fields = {\n        crap.fields.text({ name = \"title\", required = true }),\n        crap.fields.select({ name = \"status\", options = {\n            { label = \"Draft\", value = \"draft\" },\n            { label = \"Published\", value = \"published\" },\n        }}),\n        crap.fields.relationship({ name = \"author\", relationship = {\n            collection = \"users\",\n        }}),\n        crap.fields.blocks({ name = \"content\", blocks = { ... } }),\n    },\n})\n```",
    fns: [
        field_text,
        field_number,
        field_textarea,
        field_richtext,
        field_select,
        field_radio,
        field_checkbox,
        field_date,
        field_email,
        field_json,
        field_code,
        field_relationship,
        field_upload,
        field_array,
        field_group,
        field_blocks,
        field_row,
        field_collapsible,
        field_tabs,
        field_join,
    ],
}

/// Register `crap.fields.*` — per-type factory functions
/// (e.g. `crap.fields.text({ name = "title" })`). Parent `crap`
/// must already be in globals.
pub(super) fn register_fields(lua: &Lua) -> Result<()> {
    register_crap_fields(lua, ())?;
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use super::*;
    use crate::core::FieldType;
    use mlua::{Function, Lua, Table};

    fn setup_lua() -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_fields(&lua).unwrap();
        lua
    }

    fn crap_fields(lua: &Lua) -> Table {
        let crap: Table = lua.globals().get("crap").unwrap();
        crap.get("fields").unwrap()
    }

    #[test]
    fn test_fields_factory_sets_type() {
        let lua = setup_lua();
        let factory: Function = crap_fields(&lua).get("text").unwrap();
        let config = lua.create_table().unwrap();
        config.set("name", "title").unwrap();
        config.set("required", true).unwrap();
        let result: Table = factory.call(config).unwrap();
        assert_eq!(result.get::<String>("type").unwrap(), "text");
        assert_eq!(result.get::<String>("name").unwrap(), "title");
        assert!(result.get::<bool>("required").unwrap());
    }

    #[test]
    fn test_fields_factory_all_types_registered() {
        let lua = setup_lua();
        let fields = crap_fields(&lua);

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
                "Factory for '{type_name}' should set type correctly"
            );
        }
    }

    #[test]
    fn test_fields_factory_compatible_with_parse() {
        let lua = setup_lua();
        let factory: Function = crap_fields(&lua).get("select").unwrap();
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
        let parsed = crate::hooks::lua_api::parse::fields::parse_fields(&lua, &fields_arr).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "status");
        assert_eq!(parsed[0].field_type, FieldType::Select);
        assert_eq!(parsed[0].options.len(), 1);
        assert_eq!(parsed[0].options[0].value, "draft");
    }
}
