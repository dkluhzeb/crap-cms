//! Richtext before-validate hook helpers for running node attribute hooks.

use serde_json::Value;

use crate::{
    core::{DocumentFields, FieldDefinition, FieldType, Registry},
    db::query::helpers::prefixed_name,
    hooks::lifecycle::run_before_validate_on_node_attrs,
};

/// Run richtext node attr `before_validate` hooks on all richtext fields in the data map.
/// Used by both `LuaWriteHooks` and `update_many`.
/// Walks the field tree to find richtext fields with custom nodes, then runs
/// `run_before_validate_on_node_attrs` on each field's content.
pub(crate) fn apply_richtext_before_validate(
    lua: &mlua::Lua,
    fields: &[FieldDefinition],
    data: &mut DocumentFields,
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
                .is_some_and(|nd| nd.attrs.iter().any(|a| !a.hooks.before_validate.is_empty()))
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

#[cfg(test)]
mod tests {
    use crate::core::field::FieldAdmin;

    use super::*;

    /// A richtext field with a custom node registered (so it qualifies).
    fn richtext_with_nodes(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Richtext)
            .admin(FieldAdmin::builder().nodes(vec!["callout".into()]).build())
            .build()
    }

    fn plain_richtext(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Richtext).build()
    }

    fn keys(fields: &[FieldDefinition]) -> Vec<String> {
        collect_richtext_fields(fields, "")
            .into_iter()
            .map(|(_, key)| key)
            .collect()
    }

    #[test]
    fn collects_top_level_richtext_with_nodes_only() {
        let fields = vec![
            richtext_with_nodes("body"),
            plain_richtext("notes"), // no custom nodes → skipped
            FieldDefinition::builder("title", FieldType::Text).build(),
        ];
        assert_eq!(keys(&fields), vec!["body".to_string()]);
    }

    #[test]
    fn group_richtext_uses_double_underscore_data_key() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![richtext_with_nodes("body")])
                .build(),
        ];
        assert_eq!(keys(&fields), vec!["meta__body".to_string()]);
    }

    #[test]
    fn layout_wrappers_are_transparent_no_prefix() {
        let fields = vec![
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![richtext_with_nodes("body")])
                .build(),
        ];
        assert_eq!(keys(&fields), vec!["body".to_string()]);
    }

    #[test]
    fn recurses_into_tabs() {
        let fields = vec![
            FieldDefinition::builder("tabs", FieldType::Tabs)
                .tabs(vec![crate::core::field::FieldTab::new(
                    "Content",
                    vec![richtext_with_nodes("body")],
                )])
                .build(),
        ];
        assert_eq!(keys(&fields), vec!["body".to_string()]);
    }
}
