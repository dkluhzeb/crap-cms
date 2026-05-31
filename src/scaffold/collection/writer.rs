//! Lua code generation for field definitions.

use super::field_types::CONTAINER_TYPES;
use super::parser::escape_lua_string;
use super::stubs::FieldStub;

/// Write an indented line (`level * 4` spaces + format + newline) to a `String`.
macro_rules! line {
    ($out:expr, $level:expr, $($arg:tt)*) => {{
        use ::std::fmt::Write as _;
        for _ in 0..($level * 4) {
            $out.push(' ');
        }
        let _ = writeln!($out, $($arg)*);
    }};
}

/// Return type-specific Lua stub lines for non-container field types.
fn type_specific_stub(field_type: &str) -> Option<&'static str> {
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
///
/// `level` is the depth in 4-space units (top-level fields inside a
/// `crap.collections.register({ fields = { ... } })` block start at 2).
pub fn write_field_lua(lua: &mut String, field: &FieldStub, level: usize) {
    line!(lua, level, "crap.fields.{}({{", field.field_type);
    line!(
        lua,
        level + 1,
        "name = \"{}\",",
        escape_lua_string(&field.name)
    );

    if field.required {
        line!(lua, level + 1, "required = true,");
    }
    if field.localized {
        line!(lua, level + 1, "localized = true,");
    }

    write_nested_content(lua, field, level);

    line!(lua, level, "}}),");
}

/// Write nested content (fields, blocks, tabs, or default stubs) for a field.
fn write_nested_content(lua: &mut String, field: &FieldStub, level: usize) {
    if !field.fields.is_empty() {
        write_nested_fields(lua, &field.fields, level);
        return;
    }

    if !field.blocks.is_empty() {
        write_nested_blocks(lua, field, level);
        return;
    }

    if !field.tabs.is_empty() {
        write_nested_tabs(lua, field, level);
        return;
    }

    // Empty containers get default stubs
    if CONTAINER_TYPES.contains(&field.field_type.as_str()) {
        line!(
            lua,
            level + 1,
            "fields = {{ crap.fields.text({{ name = \"item\" }}) }},"
        );
        return;
    }

    if field.field_type == "blocks" {
        line!(
            lua,
            level + 1,
            "blocks = {{ {{ type = \"block_type\", label = \"Block\", fields = {{ crap.fields.text({{ name = \"content\" }}) }} }} }},"
        );
        return;
    }

    if field.field_type == "tabs" {
        line!(
            lua,
            level + 1,
            "tabs = {{ {{ label = \"Tab 1\", fields = {{ crap.fields.text({{ name = \"item\" }}) }} }} }},"
        );
        return;
    }

    if let Some(stub) = type_specific_stub(&field.field_type) {
        let pad = " ".repeat((level + 1) * 4);
        lua.push_str(&pad);
        lua.push_str(stub);
    }
}

/// Write nested `fields = { ... }` block.
fn write_nested_fields(lua: &mut String, fields: &[FieldStub], level: usize) {
    line!(lua, level + 1, "fields = {{");
    for sub in fields {
        write_field_lua(lua, sub, level + 2);
    }
    line!(lua, level + 1, "}},");
}

/// Write nested `blocks = { ... }` block.
fn write_nested_blocks(lua: &mut String, field: &FieldStub, level: usize) {
    line!(lua, level + 1, "blocks = {{");

    for block in &field.blocks {
        line!(lua, level + 2, "{{");
        line!(
            lua,
            level + 3,
            "type = \"{}\",",
            escape_lua_string(&block.block_type)
        );
        line!(
            lua,
            level + 3,
            "label = \"{}\",",
            escape_lua_string(&block.label)
        );
        line!(lua, level + 3, "fields = {{");
        for sub in &block.fields {
            write_field_lua(lua, sub, level + 4);
        }
        line!(lua, level + 3, "}},");
        line!(lua, level + 2, "}},");
    }

    line!(lua, level + 1, "}},");
}

/// Write nested `tabs = { ... }` block.
fn write_nested_tabs(lua: &mut String, field: &FieldStub, level: usize) {
    line!(lua, level + 1, "tabs = {{");

    for tab in &field.tabs {
        line!(lua, level + 2, "{{");
        line!(
            lua,
            level + 3,
            "label = \"{}\",",
            escape_lua_string(&tab.label)
        );
        line!(lua, level + 3, "fields = {{");
        for sub in &tab.fields {
            write_field_lua(lua, sub, level + 4);
        }
        line!(lua, level + 3, "}},");
        line!(lua, level + 2, "}},");
    }

    line!(lua, level + 1, "}},");
}

#[cfg(test)]
mod tests {
    use super::write_field_lua;
    use crate::scaffold::collection::stubs::FieldStub;

    fn render(field: &FieldStub) -> String {
        let mut s = String::new();
        write_field_lua(&mut s, field, 0);
        s
    }

    #[test]
    fn simple_text_field_has_name_and_no_flags() {
        let out = render(&FieldStub::builder("title", "text").build());
        assert!(out.contains("crap.fields.text({"));
        assert!(out.contains("name = \"title\","));
        assert!(!out.contains("required"));
        assert!(!out.contains("localized"));
    }

    #[test]
    fn required_and_localized_flags_are_emitted() {
        let out = render(
            &FieldStub::builder("body", "text")
                .required(true)
                .localized(true)
                .build(),
        );
        assert!(out.contains("required = true,"));
        assert!(out.contains("localized = true,"));
    }

    #[test]
    fn relationship_field_emits_its_type_specific_stub() {
        let out = render(&FieldStub::builder("author", "relationship").build());
        assert!(out.contains("crap.fields.relationship({"));
        assert!(out.contains("relationship = { collection ="));
    }

    #[test]
    fn empty_container_gets_a_default_item_stub() {
        let out = render(&FieldStub::builder("meta", "group").build());
        assert!(out.contains("crap.fields.group({"));
        assert!(out.contains(r#"fields = { crap.fields.text({ name = "item" }) }"#));
    }

    #[test]
    fn nested_fields_recurse() {
        let out = render(
            &FieldStub::builder("address", "group")
                .fields(vec![FieldStub::builder("street", "text").build()])
                .build(),
        );
        assert!(out.contains("crap.fields.group({"));
        assert!(out.contains("fields = {"));
        assert!(out.contains("crap.fields.text({"));
        assert!(out.contains("name = \"street\","));
    }
}
