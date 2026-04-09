//! Lua code generation for field definitions.

use super::parser::escape_lua_string;
use super::types::{CONTAINER_TYPES, FieldStub};

/// Return type-specific Lua stub lines for non-container field types.
pub fn type_specific_stub(field_type: &str) -> Option<&'static str> {
    match field_type {
        "select" | "radio" => {
            Some("options = { { label = \"Option 1\", value = \"option_1\" } },\n")
        }
        "relationship" => Some(
            "relationship = { collection = \"other_collection\" }, -- change to target collection slug\n",
        ),
        "upload" => Some("relationship = { collection = \"media\" },\n"),
        "join" => Some(
            "collection = \"other_collection\", -- target collection slug\n            on = \"field_name\",               -- relationship field on target that points back\n",
        ),
        "code" => Some("admin = { language = \"javascript\" },\n"),
        _ => None,
    }
}

/// Write a single field's Lua representation with proper indentation and recursion.
pub fn write_field_lua(lua: &mut String, field: &FieldStub, indent: usize) {
    let pad = " ".repeat(indent);
    let inner = " ".repeat(indent + 4);

    lua.push_str(&format!("{}crap.fields.{}({{\n", pad, field.field_type));
    lua.push_str(&format!(
        "{}name = \"{}\",\n",
        inner,
        escape_lua_string(&field.name)
    ));

    if field.required {
        lua.push_str(&format!("{}required = true,\n", inner));
    }
    if field.localized {
        lua.push_str(&format!("{}localized = true,\n", inner));
    }

    write_nested_content(lua, field, indent);

    lua.push_str(&format!("{}}}),\n", pad));
}

/// Write nested content (fields, blocks, tabs, or default stubs) for a field.
fn write_nested_content(lua: &mut String, field: &FieldStub, indent: usize) {
    let inner = " ".repeat(indent + 4);

    if !field.fields.is_empty() {
        write_nested_fields(lua, &field.fields, indent);
        return;
    }

    if !field.blocks.is_empty() {
        write_nested_blocks(lua, field, indent);
        return;
    }

    if !field.tabs.is_empty() {
        write_nested_tabs(lua, field, indent);
        return;
    }

    // Empty containers get default stubs
    if CONTAINER_TYPES.contains(&field.field_type.as_str()) {
        lua.push_str(&format!(
            "{}fields = {{ crap.fields.text({{ name = \"item\" }}) }},\n",
            inner
        ));
        return;
    }

    if field.field_type == "blocks" {
        lua.push_str(&format!(
            "{}blocks = {{ {{ type = \"block_type\", label = \"Block\", fields = {{ crap.fields.text({{ name = \"content\" }}) }} }} }},\n",
            inner
        ));
        return;
    }

    if field.field_type == "tabs" {
        lua.push_str(&format!(
            "{}tabs = {{ {{ label = \"Tab 1\", fields = {{ crap.fields.text({{ name = \"item\" }}) }} }} }},\n",
            inner
        ));
        return;
    }

    if let Some(stub) = type_specific_stub(&field.field_type) {
        lua.push_str(&format!("{}{}", inner, stub));
    }
}

/// Write nested `fields = { ... }` block.
fn write_nested_fields(lua: &mut String, fields: &[FieldStub], indent: usize) {
    let inner = " ".repeat(indent + 4);

    lua.push_str(&format!("{}fields = {{\n", inner));
    for sub in fields {
        write_field_lua(lua, sub, indent + 8);
    }
    lua.push_str(&format!("{}}},\n", inner));
}

/// Write nested `blocks = { ... }` block.
fn write_nested_blocks(lua: &mut String, field: &FieldStub, indent: usize) {
    let inner = " ".repeat(indent + 4);
    let block_pad = " ".repeat(indent + 8);
    let block_inner = " ".repeat(indent + 12);

    lua.push_str(&format!("{}blocks = {{\n", inner));

    for block in &field.blocks {
        lua.push_str(&format!("{}{{\n", block_pad));
        lua.push_str(&format!(
            "{}type = \"{}\",\n",
            block_inner,
            escape_lua_string(&block.block_type)
        ));
        lua.push_str(&format!(
            "{}label = \"{}\",\n",
            block_inner,
            escape_lua_string(&block.label)
        ));
        lua.push_str(&format!("{}fields = {{\n", block_inner));
        for sub in &block.fields {
            write_field_lua(lua, sub, indent + 16);
        }
        lua.push_str(&format!("{}}},\n", block_inner));
        lua.push_str(&format!("{}}},\n", block_pad));
    }

    lua.push_str(&format!("{}}},\n", inner));
}

/// Write nested `tabs = { ... }` block.
fn write_nested_tabs(lua: &mut String, field: &FieldStub, indent: usize) {
    let inner = " ".repeat(indent + 4);
    let tab_pad = " ".repeat(indent + 8);
    let tab_inner = " ".repeat(indent + 12);

    lua.push_str(&format!("{}tabs = {{\n", inner));

    for tab in &field.tabs {
        lua.push_str(&format!("{}{{\n", tab_pad));
        lua.push_str(&format!(
            "{}label = \"{}\",\n",
            tab_inner,
            escape_lua_string(&tab.label)
        ));
        lua.push_str(&format!("{}fields = {{\n", tab_inner));
        for sub in &tab.fields {
            write_field_lua(lua, sub, indent + 16);
        }
        lua.push_str(&format!("{}}},\n", tab_inner));
        lua.push_str(&format!("{}}},\n", tab_pad));
    }

    lua.push_str(&format!("{}}},\n", inner));
}
