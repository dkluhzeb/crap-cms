//! Parsing functions for block and tab definitions.

use anyhow::{Context as _, Result, anyhow};
use mlua::{Lua, Table};

use crate::core::{BlockDefinition, FieldTab};
use crate::db::query::validate_slug;

use super::{
    fields::parse_fields,
    helpers::{deny_unknown_keys, get_localized_string, get_string, get_string_val, get_table},
};

pub(super) fn parse_block_definitions(
    lua: &Lua,
    blocks_tbl: &Table,
) -> Result<Vec<BlockDefinition>> {
    let mut blocks = Vec::new();

    for entry in blocks_tbl.clone().sequence_values::<Table>() {
        let def = entry?;
        deny_unknown_keys(
            &def,
            "block",
            &[
                "type",
                "label",
                "label_field",
                "group",
                "image_url",
                "fields",
            ],
        )?;
        let block_type: String =
            get_string_val(&def, "type").map_err(|_| anyhow!("Block definition missing 'type'"))?;

        // A block `type` is a discriminator (stored in `_block_type`) and a
        // dotted-access key, so it follows the same slug rules as every other
        // identifier (field/node/collection/job names) rather than being the one
        // unvalidated identifier in the schema.
        validate_slug(&block_type).with_context(|| format!("Invalid block type '{block_type}'"))?;
        let label = get_localized_string(&def, "label");
        let label_field = get_string(&def, "label_field");
        let group = get_string(&def, "group");
        let image_url = get_string(&def, "image_url");
        let fields = if let Ok(fields_tbl) = get_table(&def, "fields") {
            parse_fields(lua, &fields_tbl)?
        } else {
            Vec::new()
        };
        let mut block = BlockDefinition::new(block_type, fields);

        block.label = label;
        block.label_field = label_field;
        block.group = group;
        block.image_url = image_url;
        blocks.push(block);
    }

    Ok(blocks)
}

pub(super) fn parse_tab_definitions(lua: &Lua, tabs_tbl: &Table) -> Result<Vec<FieldTab>> {
    let mut tabs = Vec::new();

    for entry in tabs_tbl.clone().sequence_values::<Table>() {
        let def = entry?;
        deny_unknown_keys(&def, "tab", &["label", "description", "fields"])?;
        let label = get_string(&def, "label").unwrap_or_default();
        let description = get_string(&def, "description");
        let fields = if let Ok(fields_tbl) = get_table(&def, "fields") {
            parse_fields(lua, &fields_tbl)?
        } else {
            Vec::new()
        };
        let mut tab = FieldTab::new(label, fields);

        tab.description = description;
        tabs.push(tab);
    }
    Ok(tabs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    #[test]
    fn test_parse_fields_blocks_type() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "content").unwrap();
        field.set("type", "blocks").unwrap();
        let blocks = lua.create_table().unwrap();
        let block = lua.create_table().unwrap();
        block.set("type", "paragraph").unwrap();
        block.set("label", "Paragraph").unwrap();
        let bfields = lua.create_table().unwrap();
        let bf = lua.create_table().unwrap();
        bf.set("name", "text").unwrap();
        bf.set("type", "textarea").unwrap();
        bfields.set(1, bf).unwrap();
        block.set("fields", bfields).unwrap();
        blocks.set(1, block).unwrap();
        field.set("blocks", blocks).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
        assert_eq!(fields[0].blocks.len(), 1);
        assert_eq!(fields[0].blocks[0].block_type, "paragraph");
        assert_eq!(fields[0].blocks[0].fields.len(), 1);
        assert_eq!(fields[0].blocks[0].fields[0].name, "text");
    }

    /// Regression: a block `type` must be a valid slug, like every other
    /// identifier — a hyphenated / spaced / camelCase type is rejected at load
    /// rather than being the one unvalidated identifier in the schema.
    #[test]
    fn parse_block_definitions_rejects_invalid_type() {
        let lua = Lua::new();

        for bad in ["hero-image", "Hero", "call to action", "_hidden"] {
            let blocks_tbl = lua.create_table().unwrap();
            let block = lua.create_table().unwrap();
            block.set("type", bad).unwrap();
            blocks_tbl.set(1, block).unwrap();

            let err = parse_block_definitions(&lua, &blocks_tbl)
                .expect_err(&format!("block type '{bad}' should be rejected"));
            assert!(
                err.to_string().contains("block type") || err.to_string().contains("slug"),
                "unexpected error for '{bad}': {err}"
            );
        }
    }

    #[test]
    fn test_parse_block_definitions_optional_fields() {
        let lua = Lua::new();
        let blocks_tbl = lua.create_table().unwrap();
        let block = lua.create_table().unwrap();
        block.set("type", "hero").unwrap();
        block.set("label_field", "headline").unwrap();
        block.set("group", "Layout").unwrap();
        block
            .set("image_url", "https://example.com/hero.png")
            .unwrap();
        blocks_tbl.set(1, block).unwrap();
        let blocks = parse_block_definitions(&lua, &blocks_tbl).unwrap();
        assert_eq!(blocks[0].label_field.as_deref(), Some("headline"));
        assert_eq!(blocks[0].group.as_deref(), Some("Layout"));
        assert_eq!(
            blocks[0].image_url.as_deref(),
            Some("https://example.com/hero.png")
        );
    }

    #[test]
    fn test_parse_block_definitions_missing_type_error() {
        let lua = Lua::new();
        let blocks_tbl = lua.create_table().unwrap();
        let block = lua.create_table().unwrap();
        blocks_tbl.set(1, block).unwrap();
        let result = parse_block_definitions(&lua, &blocks_tbl);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing 'type'"));
    }

    #[test]
    fn test_parse_fields_tabs_type() {
        let lua = Lua::new();
        let fields_tbl = lua.create_table().unwrap();
        let field = lua.create_table().unwrap();
        field.set("name", "tabbed_section").unwrap();
        field.set("type", "tabs").unwrap();
        let tabs = lua.create_table().unwrap();
        let tab = lua.create_table().unwrap();
        tab.set("label", "General").unwrap();
        tab.set("description", "General settings").unwrap();
        let tfields = lua.create_table().unwrap();
        let tf = lua.create_table().unwrap();
        tf.set("name", "bio").unwrap();
        tf.set("type", "textarea").unwrap();
        tfields.set(1, tf).unwrap();
        tab.set("fields", tfields).unwrap();
        tabs.set(1, tab).unwrap();
        field.set("tabs", tabs).unwrap();
        fields_tbl.set(1, field).unwrap();
        let fields = parse_fields(&lua, &fields_tbl).unwrap();
        assert_eq!(fields[0].tabs.len(), 1);
        assert_eq!(fields[0].tabs[0].label, "General");
        assert_eq!(
            fields[0].tabs[0].description.as_deref(),
            Some("General settings")
        );
        assert_eq!(fields[0].tabs[0].fields.len(), 1);
    }
}
