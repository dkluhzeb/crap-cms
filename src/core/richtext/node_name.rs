//! The rule for a custom richtext node's name — one validator shared by
//! `crap.richtext.register_node` and the `make node` scaffold, so the
//! scaffold never writes a registration the runtime rejects at boot.

/// Built-in `ProseMirror` node types. A custom node with one of these names
/// would silently fail at render time — the built-in match arm in
/// `core::richtext::renderer::render_node` runs first and the custom
/// renderer is never called — so the name is rejected.
pub const RESERVED_NODE_NAMES: &[&str] = &[
    "doc",
    "paragraph",
    "text",
    "heading",
    "blockquote",
    "code_block",
    "bullet_list",
    "ordered_list",
    "list_item",
    "horizontal_rule",
    "hard_break",
];

/// Validate a custom node name: non-empty, only lowercase ASCII letters,
/// digits and underscores, not starting with a digit or underscore, and not
/// a built-in `ProseMirror` node type.
///
/// # Errors
///
/// Returns the message naming what is wrong with `name`.
pub fn validate_node_name(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit() || c == '_');

    if !valid {
        return Err(format!(
            "Invalid node name '{name}': must be non-empty, use only lowercase ASCII letters, \
             digits, and underscores, and not start with a digit or underscore"
        ));
    }

    if RESERVED_NODE_NAMES.contains(&name) {
        return Err(format!(
            "Invalid node name '{name}': collides with a built-in ProseMirror node type. \
             Built-in names {RESERVED_NODE_NAMES:?} are reserved — pick a different name."
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_lowercase_identifiers() {
        assert!(validate_node_name("cta").is_ok());
        assert!(validate_node_name("call_to_action2").is_ok());
    }

    #[test]
    fn rejects_bad_shapes_and_reserved_names() {
        for bad in [
            "",
            "2col",
            "_x",
            "Cta",
            "my-node",
            "paragraph",
            "hard_break",
        ] {
            assert!(validate_node_name(bad).is_err(), "{bad} must be rejected");
        }
    }
}
