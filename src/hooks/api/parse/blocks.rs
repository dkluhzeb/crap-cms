//! Parsing functions for block and tab definitions.

use anyhow::Result;
use mlua::Table;

use super::helpers::*;
use super::fields::parse_fields;

pub(super) fn parse_block_definitions(blocks_tbl: &Table) -> Result<Vec<crate::core::field::BlockDefinition>> {
    let mut blocks = Vec::new();
    for entry in blocks_tbl.clone().sequence_values::<Table>() {
        let block_tbl = entry?;
        let block_type: String = get_string_val(&block_tbl, "type")
            .map_err(|_| anyhow::anyhow!("Block definition missing 'type'"))?;
        let label = get_localized_string(&block_tbl, "label");
        let label_field = get_string(&block_tbl, "label_field");
        let group = get_string(&block_tbl, "group");
        let image_url = get_string(&block_tbl, "image_url");
        let fields = if let Ok(fields_tbl) = get_table(&block_tbl, "fields") {
            parse_fields(&fields_tbl)?
        } else {
            Vec::new()
        };
        let mut block = crate::core::field::BlockDefinition::new(block_type, fields);
        block.label = label;
        block.label_field = label_field;
        block.group = group;
        block.image_url = image_url;
        blocks.push(block);
    }
    Ok(blocks)
}

pub(super) fn parse_tab_definitions(tabs_tbl: &Table) -> Result<Vec<crate::core::field::FieldTab>> {
    let mut tabs = Vec::new();
    for entry in tabs_tbl.clone().sequence_values::<Table>() {
        let tab_tbl = entry?;
        let label = get_string(&tab_tbl, "label").unwrap_or_default();
        let description = get_string(&tab_tbl, "description");
        let fields = if let Ok(fields_tbl) = get_table(&tab_tbl, "fields") {
            parse_fields(&fields_tbl)?
        } else {
            Vec::new()
        };
        let mut tab = crate::core::field::FieldTab::new(label, fields);
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
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].blocks.len(), 1);
        assert_eq!(fields[0].blocks[0].block_type, "paragraph");
        assert_eq!(fields[0].blocks[0].fields.len(), 1);
        assert_eq!(fields[0].blocks[0].fields[0].name, "text");
    }

    #[test]
    fn test_parse_block_definitions_optional_fields() {
        let lua = Lua::new();
        let blocks_tbl = lua.create_table().unwrap();
        let block = lua.create_table().unwrap();
        block.set("type", "hero").unwrap();
        block.set("label_field", "headline").unwrap();
        block.set("group", "Layout").unwrap();
        block.set("image_url", "https://example.com/hero.png").unwrap();
        blocks_tbl.set(1, block).unwrap();
        let blocks = parse_block_definitions(&blocks_tbl).unwrap();
        assert_eq!(blocks[0].label_field.as_deref(), Some("headline"));
        assert_eq!(blocks[0].group.as_deref(), Some("Layout"));
        assert_eq!(blocks[0].image_url.as_deref(), Some("https://example.com/hero.png"));
    }

    #[test]
    fn test_parse_block_definitions_missing_type_error() {
        let lua = Lua::new();
        let blocks_tbl = lua.create_table().unwrap();
        let block = lua.create_table().unwrap();
        blocks_tbl.set(1, block).unwrap();
        let result = parse_block_definitions(&blocks_tbl);
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
        let fields = parse_fields(&fields_tbl).unwrap();
        assert_eq!(fields[0].tabs.len(), 1);
        assert_eq!(fields[0].tabs[0].label, "General");
        assert_eq!(fields[0].tabs[0].description.as_deref(), Some("General settings"));
        assert_eq!(fields[0].tabs[0].fields.len(), 1);
    }
}
