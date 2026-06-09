//! `before_validate` hook pipeline for richtext node attrs. Walks `ProseMirror`
//! JSON or HTML and runs each node's per-attr `before_validate` Lua hooks,
//! transforming the attr values in-place.

use std::collections::HashMap;
use std::fmt::Write as _;

use mlua::Lua;
use serde_json::Value;

use crate::{
    core::{
        FieldDefinition, HookRef, Registry,
        richtext::renderer::{extract_attr_value, html_escape_attr},
    },
    hooks::{lifecycle::execution::resolve_hook_function, lua_api},
};

///
/// Returns the (potentially modified) content string.
pub(crate) fn run_before_validate_on_node_attrs(
    lua: &Lua,
    content: &str,
    field: &FieldDefinition,
    registry: &Registry,
    collection: &str,
) -> String {
    let format = field.admin.richtext_format.as_deref().unwrap_or("html");

    // Check if any node attr has before_validate hooks
    let has_hooks = field.admin.nodes.iter().any(|node_name| {
        registry
            .get_richtext_node(node_name)
            .is_some_and(|nd| nd.attrs.iter().any(|a| !a.hooks.before_validate.is_empty()))
    });

    if !has_hooks {
        return content.to_string();
    }

    if format == "json" {
        run_before_validate_json(lua, content, field, registry, collection)
    } else {
        run_before_validate_html(lua, content, field, registry, collection)
    }
}

/// Run `before_validate` hooks on node attrs in `ProseMirror` JSON content.
fn run_before_validate_json(
    lua: &Lua,
    content: &str,
    field: &FieldDefinition,
    registry: &Registry,
    collection: &str,
) -> String {
    let mut parsed: Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return content.to_string(),
    };

    let mut modified = false;
    transform_nodes_json(&mut parsed, field, registry, lua, collection, &mut modified);

    if modified {
        serde_json::to_string(&parsed).unwrap_or_else(|_| content.to_string())
    } else {
        content.to_string()
    }
}

fn transform_nodes_json(
    value: &mut Value,
    field: &FieldDefinition,
    registry: &Registry,
    lua: &Lua,
    collection: &str,
    modified: &mut bool,
) {
    let node_type = value
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    // Check if this is a known custom node with before_validate hooks
    if field.admin.nodes.contains(&node_type)
        && let Some(node_def) = registry.get_richtext_node(&node_type)
        && let Some(attrs) = value.get_mut("attrs").and_then(|a| a.as_object_mut())
    {
        for attr_def in &node_def.attrs {
            if attr_def.hooks.before_validate.is_empty() {
                continue;
            }
            if let Some(attr_val) = attrs.get(&attr_def.name).cloned() {
                let new_val = run_attr_before_validate_hooks(
                    lua,
                    &attr_def.hooks.before_validate,
                    &attr_val,
                    collection,
                    &attr_def.name,
                );
                if new_val != attr_val {
                    attrs.insert(attr_def.name.clone(), new_val);
                    *modified = true;
                }
            }
        }
    }

    // Recurse into children
    if let Some(content) = value.get_mut("content").and_then(|c| c.as_array_mut()) {
        for child in content {
            transform_nodes_json(child, field, registry, lua, collection, modified);
        }
    }
}

/// Run `before_validate` hooks on node attrs in HTML content.
fn run_before_validate_html(
    lua: &Lua,
    content: &str,
    field: &FieldDefinition,
    registry: &Registry,
    collection: &str,
) -> String {
    let mut result = String::with_capacity(content.len());
    let mut remaining = content;

    while let Some(start) = remaining.find("<crap-node ") {
        result.push_str(&remaining[..start]);
        let after_start = &remaining[start..];

        let close_pos = match (after_start.find("/>"), after_start.find("</crap-node>")) {
            (Some(sc), Some(et)) => {
                if sc < et {
                    sc + "/>".len()
                } else {
                    et + "</crap-node>".len()
                }
            }
            (Some(sc), None) => sc + "/>".len(),
            (None, Some(et)) => et + "</crap-node>".len(),
            (None, None) => {
                result.push_str(after_start);
                remaining = "";
                continue;
            }
        };

        let tag = &after_start[..close_pos];

        if let Some(node_type) = extract_attr_value(tag, "data-type")
            && field.admin.nodes.contains(&node_type)
            && let Some(node_def) = registry.get_richtext_node(&node_type)
            && node_def
                .attrs
                .iter()
                .any(|a| !a.hooks.before_validate.is_empty())
        {
            let mut attrs: HashMap<String, Value> = extract_attr_value(tag, "data-attrs")
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();

            let mut changed = false;
            for attr_def in &node_def.attrs {
                if attr_def.hooks.before_validate.is_empty() {
                    continue;
                }
                if let Some(attr_val) = attrs.get(&attr_def.name).cloned() {
                    let new_val = run_attr_before_validate_hooks(
                        lua,
                        &attr_def.hooks.before_validate,
                        &attr_val,
                        collection,
                        &attr_def.name,
                    );
                    if new_val != attr_val {
                        attrs.insert(attr_def.name.clone(), new_val);
                        changed = true;
                    }
                }
            }

            if changed {
                let attrs_json = serde_json::to_string(&attrs).unwrap_or_default();
                let _ = write!(
                    result,
                    "<crap-node data-type=\"{}\" data-attrs='{}'></crap-node>",
                    html_escape_attr(&node_type),
                    html_escape_attr(&attrs_json),
                );
            } else {
                result.push_str(tag);
            }
        } else {
            result.push_str(tag);
        }

        remaining = &after_start[close_pos..];
    }

    result.push_str(remaining);
    result
}

/// Run a chain of `before_validate` hook functions on a single attr value.
fn run_attr_before_validate_hooks(
    lua: &Lua,
    hook_refs: &[HookRef],
    value: &Value,
    collection: &str,
    field_name: &str,
) -> Value {
    let mut current = value.clone();
    for hook in hook_refs {
        let hook_ref = hook.reference();
        let func = match resolve_hook_function(lua, hook_ref) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    "before_validate hook '{}' for node attr '{}' not found: {}",
                    hook_ref,
                    field_name,
                    e,
                );
                continue;
            }
        };
        let Ok(lua_val) = lua_api::json_to_lua(lua, &current) else {
            continue;
        };
        let Ok(ctx_table) = lua.create_table() else {
            continue;
        };
        let _ = ctx_table.set("collection", collection);
        let _ = ctx_table.set("field_name", field_name);

        if let Some(opts) = hook.options()
            && let Ok(opts_val) = lua_api::json_to_lua(lua, opts)
        {
            let _ = ctx_table.set("options", opts_val);
        }

        match func.call::<mlua::Value>((lua_val, ctx_table)) {
            Ok(result) => {
                if let Ok(json_val) = lua_api::lua_to_json(&result) {
                    current = json_val;
                }
            }
            Err(e) => {
                tracing::warn!(
                    "before_validate hook '{}' for node attr '{}' failed: {}",
                    hook_ref,
                    field_name,
                    e,
                );
            }
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use crate::core::field::{FieldAdmin, FieldType};

    use super::*;

    fn richtext_field(nodes: Vec<String>) -> FieldDefinition {
        FieldDefinition::builder("body", FieldType::Richtext)
            .admin(FieldAdmin::builder().nodes(nodes).build())
            .build()
    }

    /// With no custom nodes, the short-circuit returns the content verbatim —
    /// crucially WITHOUT parsing it, so even invalid JSON/HTML passes through
    /// untouched (and no Lua runs).
    #[test]
    fn no_custom_nodes_returns_content_verbatim() {
        let lua = Lua::new();
        let registry = Registry::new();
        let field = richtext_field(vec![]);

        let garbage = "{ this is not valid json <crap-node";
        assert_eq!(
            run_before_validate_on_node_attrs(&lua, garbage, &field, &registry, "posts"),
            garbage
        );
    }

    /// A node name is declared on the field but the registry has no matching
    /// node definition (hence no `before_validate` hooks) → still a pass-through.
    #[test]
    fn declared_node_absent_from_registry_is_pass_through() {
        let lua = Lua::new();
        let registry = Registry::new();
        let field = richtext_field(vec!["callout".into()]);

        let content = r#"{"type":"doc","content":[]}"#;
        assert_eq!(
            run_before_validate_on_node_attrs(&lua, content, &field, &registry, "posts"),
            content
        );
    }
}
