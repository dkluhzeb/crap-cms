//! Richtext before-validate hook helpers for running node attribute hooks.

use serde_json::Value;

use crate::{
    core::{FieldDefinition, FieldType, Registry},
    db::query::helpers::prefixed_name,
    hooks::lifecycle::run_before_validate_on_node_attrs,
};

/// Run richtext node attr before_validate hooks on all richtext fields in the data map.
/// Used by both `LuaWriteHooks` and `update_many`.
/// Walks the field tree to find richtext fields with custom nodes, then runs
/// `run_before_validate_on_node_attrs` on each field's content.
pub(crate) fn apply_richtext_before_validate(
    lua: &mlua::Lua,
    fields: &[FieldDefinition],
    data: &mut std::collections::HashMap<String, Value>,
    registry: &Registry,
    collection: &str,
) {
    let richtext_fields = collect_richtext_fields(fields, "");

    if richtext_fields.is_empty() {
        return;
    }

    let has_any_hooks = richtext_fields.iter().any(|(f, _)| {
        f.admin.nodes.iter().any(|node_name| {
            registry
                .get_richtext_node(node_name)
                .map(|nd| nd.attrs.iter().any(|a| !a.hooks.before_validate.is_empty()))
                .unwrap_or(false)
        })
    });

    if !has_any_hooks {
        return;
    }

    for (field, data_key) in &richtext_fields {
        if let Some(Value::String(content)) = data.get(data_key.as_str()) {
            let new_content =
                run_before_validate_on_node_attrs(lua, content, field, registry, collection);
            if new_content != *content {
                data.insert(data_key.clone(), Value::String(new_content));
            }
        }
    }
}

/// Walk the field tree and collect richtext fields with custom nodes.
fn collect_richtext_fields<'a>(
    fields: &'a [FieldDefinition],
    prefix: &str,
) -> Vec<(&'a FieldDefinition, String)> {
    let mut out = Vec::new();

    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = prefixed_name(prefix, &field.name);
                out.extend(collect_richtext_fields(&field.fields, &new_prefix));
            }
            FieldType::Row | FieldType::Collapsible => {
                out.extend(collect_richtext_fields(&field.fields, prefix));
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    out.extend(collect_richtext_fields(&tab.fields, prefix));
                }
            }
            FieldType::Richtext if !field.admin.nodes.is_empty() => {
                out.push((field, prefixed_name(prefix, &field.name)));
            }
            _ => {}
        }
    }

    out
}
