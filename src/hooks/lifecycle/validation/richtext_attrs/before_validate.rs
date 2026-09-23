//! `before_validate` hook pipeline for richtext node attrs. Walks `ProseMirror`
//! JSON or HTML and runs each node's per-attr `before_validate` Lua hooks,
//! transforming the attr values in-place.

use std::fmt::Write as _;

use mlua::Lua;
use serde_json::Value;

use crate::{
    core::{
        DocumentFields, FieldDefinition, FieldType, HookRef, Registry, VisitAction, any_field,
        richtext::{CrapNodeTag, find_crap_nodes, renderer::html_escape_attr},
        walk_nested_mut,
    },
    hooks::{lifecycle::execution::resolve_hook_function, lua_api},
};

use super::extract::json_document;

/// Whether `field` is a rich text field using a node whose attrs carry
/// `before_validate` hooks.
fn field_has_attr_hooks(field: &FieldDefinition, registry: &Registry) -> bool {
    field.field_type == FieldType::Richtext
        && field.admin.nodes.iter().any(|node_name| {
            registry
                .get_richtext_node(node_name)
                .is_some_and(|nd| nd.attrs.iter().any(|a| !a.hooks.before_validate.is_empty()))
        })
}

/// Whether any rich text field in `fields`, at any depth (groups, array and
/// blocks rows, layout wrappers), has node-attr `before_validate` hooks — the
/// cheap probe a caller runs before acquiring a VM.
pub(crate) fn has_node_attr_before_validate(
    fields: &[FieldDefinition],
    registry: &Registry,
) -> bool {
    any_field(fields, &|field| field_has_attr_hooks(field, registry))
}

/// Run node-attr `before_validate` hooks on every rich text value in `data`:
/// top level, groups and array/blocks rows at any depth, over the canonical
/// nested write shape. The one pass every write-hook implementation runs.
pub(crate) fn apply_node_attr_before_validate(
    lua: &Lua,
    fields: &[FieldDefinition],
    data: &mut DocumentFields,
    registry: &Registry,
    collection: &str,
) {
    if !has_node_attr_before_validate(fields, registry) {
        return;
    }

    walk_nested_mut(data, fields, &mut Vec::new(), &mut |field, level, _| {
        if !field_has_attr_hooks(field, registry) {
            return VisitAction::Keep;
        }

        level
            .root_get(&field.name)
            .and_then(|value| transform_richtext_value(lua, value, field, registry, collection))
            .map_or(VisitAction::Keep, VisitAction::Replace)
    });
}

/// One rich text value after its node-attr hooks, or `None` when unchanged. A
/// JSON-format document may be its text or the document object; either keeps
/// its shape.
fn transform_richtext_value(
    lua: &Lua,
    value: &Value,
    field: &FieldDefinition,
    registry: &Registry,
    collection: &str,
) -> Option<Value> {
    match value {
        Value::String(content) => {
            let new_content =
                run_before_validate_on_node_attrs(lua, content, field, registry, collection);
            (new_content != *content).then_some(Value::String(new_content))
        }
        Value::Object(_) if field.parses_json() => {
            // A value that is not a readable document (e.g. nested past the
            // depth limit) is left for validation to refuse.
            json_document(value)?;

            let mut doc = value.clone();
            let mut modified = false;
            transform_nodes_json(&mut doc, field, registry, lua, collection, &mut modified);
            modified.then_some(doc)
        }
        _ => None,
    }
}

/// Run node-attr `before_validate` hooks over one rich text content string
/// (`ProseMirror` JSON text or HTML, per the field's format).
///
/// Returns the (potentially modified) content string.
pub(crate) fn run_before_validate_on_node_attrs(
    lua: &Lua,
    content: &str,
    field: &FieldDefinition,
    registry: &Registry,
    collection: &str,
) -> String {
    if !field_has_attr_hooks(field, registry) {
        return content.to_string();
    }

    if field.parses_json() {
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

/// Run `before_validate` hooks on node attrs in HTML content. Nodes are found
/// with the tokenizer validation and the renderer use; a node whose attrs a
/// hook changed is rewritten, everything else is copied verbatim.
fn run_before_validate_html(
    lua: &Lua,
    content: &str,
    field: &FieldDefinition,
    registry: &Registry,
    collection: &str,
) -> String {
    let mut result = String::with_capacity(content.len());
    let mut copied = 0;

    for tag in find_crap_nodes(content) {
        let Some(rewritten) = rewrite_html_node(lua, &tag, field, registry, collection) else {
            continue;
        };

        result.push_str(&content[copied..tag.start]);
        result.push_str(&rewritten);
        copied = tag.end;
    }

    result.push_str(&content[copied..]);
    result
}

/// The node's markup after its attr hooks ran, or `None` when the node has no
/// hooks or they changed nothing.
fn rewrite_html_node(
    lua: &Lua,
    tag: &CrapNodeTag,
    field: &FieldDefinition,
    registry: &Registry,
    collection: &str,
) -> Option<String> {
    let node_type = tag.node_type()?;
    if !field.admin.nodes.iter().any(|n| n == node_type) {
        return None;
    }

    let node_def = registry.get_richtext_node(node_type)?;
    let mut attrs = tag.node_attrs();
    let mut changed = false;

    for attr_def in &node_def.attrs {
        if attr_def.hooks.before_validate.is_empty() {
            continue;
        }

        let Some(attr_val) = attrs.get(&attr_def.name).cloned() else {
            continue;
        };

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

    if !changed {
        return None;
    }

    let attrs_json = serde_json::to_string(&attrs).unwrap_or_default();
    let mut out = String::new();
    let _ = write!(
        out,
        "<crap-node data-type=\"{}\" data-attrs='{}'></crap-node>",
        html_escape_attr(node_type),
        html_escape_attr(&attrs_json),
    );

    Some(out)
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
    use serde_json::json;

    use crate::core::{
        FieldHooks, RichtextNodeDef,
        field::{FieldAdmin, FieldType},
    };

    use super::*;

    fn formatted_field(nodes: Vec<String>, format: &str) -> FieldDefinition {
        FieldDefinition::builder("content", FieldType::Richtext)
            .admin(
                FieldAdmin::builder()
                    .richtext_format(format)
                    .nodes(nodes)
                    .build(),
            )
            .build()
    }

    fn cta_registry() -> Registry {
        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("cta", "CTA")
                .attrs(vec![
                    FieldDefinition::builder("text", FieldType::Text).build(),
                    FieldDefinition::builder("url", FieldType::Text).build(),
                ])
                .build(),
        );
        reg
    }

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

    // ── apply_node_attr_before_validate ────────────────────────────────────

    /// A VM with a `hooks.trim` function and a registry whose `note` node's
    /// `text` attr runs it before validation.
    fn trimming_setup() -> (Lua, Registry) {
        let lua = Lua::new();
        lua.load(
            r#"package.loaded["hooks"] = {
                trim = function(value) return (value:gsub("^%s+", ""):gsub("%s+$", "")) end
            }"#,
        )
        .exec()
        .unwrap();

        let mut registry = Registry::new();
        registry.register_richtext_node(
            RichtextNodeDef::builder("note", "Note")
                .attrs(vec![
                    FieldDefinition::builder("text", FieldType::Text)
                        .hooks(FieldHooks {
                            before_validate: vec![HookRef::new("hooks.trim")],
                            ..Default::default()
                        })
                        .build(),
                ])
                .build(),
        );

        (lua, registry)
    }

    fn note_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Richtext)
            .admin(
                FieldAdmin::builder()
                    .nodes(vec!["note".into()])
                    .richtext_format("json")
                    .build(),
            )
            .build()
    }

    fn note_doc(text: &str) -> Value {
        json!({ "type": "doc", "content": [{ "type": "note", "attrs": { "text": text } }] })
    }

    /// Regression: the hooks were looked up under flat `group__field` keys,
    /// but write data is nested by then, so a rich text field in a group never
    /// had its node-attr hooks run.
    #[test]
    fn hooks_run_for_rich_text_inside_a_group() {
        let (lua, registry) = trimming_setup();
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![note_field("body")])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert(
            "meta".into(),
            json!({ "body": note_doc("  hi  ").to_string() }),
        );

        apply_node_attr_before_validate(&lua, &fields, &mut data, &registry, "posts");

        let body: Value = serde_json::from_str(data["meta"]["body"].as_str().unwrap()).unwrap();
        assert_eq!(body, note_doc("hi"));
    }

    /// Regression: rich text inside array/blocks rows never ran its hooks.
    #[test]
    fn hooks_run_for_rich_text_inside_array_rows() {
        let (lua, registry) = trimming_setup();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![note_field("body")])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("items".into(), json!([{ "body": note_doc("  a  ") }]));

        apply_node_attr_before_validate(&lua, &fields, &mut data, &registry, "posts");

        assert_eq!(
            data["items"][0]["body"],
            note_doc("a"),
            "an object stays an object"
        );
    }

    #[test]
    fn nested_fields_without_hooks_are_probed_false() {
        let registry = Registry::new();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![note_field("body")])
                .build(),
        ];

        assert!(!has_node_attr_before_validate(&fields, &registry));
        assert!(has_node_attr_before_validate(&fields, &trimming_setup().1));
    }

    #[test]
    fn before_validate_hooks_transform_json() {
        let lua = Lua::new();
        lua.load(
            r#"
            package.loaded["hooks"] = {
                trim = function(value, ctx)
                    if type(value) == "string" then
                        return value:match("^%s*(.-)%s*$")
                    end
                    return value
                end
            }
        "#,
        )
        .exec()
        .unwrap();

        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("note", "Note")
                .attrs(vec![
                    FieldDefinition::builder("text", FieldType::Text)
                        .hooks(FieldHooks {
                            before_validate: vec![HookRef::new("hooks.trim")],
                            ..Default::default()
                        })
                        .build(),
                ])
                .build(),
        );
        let field = formatted_field(vec!["note".to_string()], "json");
        let content = r#"{"type":"doc","content":[{"type":"note","attrs":{"text":"  hello  "}}]}"#;

        let result = run_before_validate_on_node_attrs(&lua, content, &field, &reg, "pages");

        let parsed: Value = serde_json::from_str(&result).unwrap();
        let text = parsed["content"][0]["attrs"]["text"].as_str().unwrap();
        assert_eq!(text, "hello");
    }

    #[test]
    fn before_validate_hooks_no_hooks_returns_original() {
        let lua = Lua::new();
        let reg = cta_registry();
        let field = formatted_field(vec!["cta".to_string()], "json");
        let content =
            r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"hi","url":"/"}}]}"#;

        let result = run_before_validate_on_node_attrs(&lua, content, &field, &reg, "pages");
        assert_eq!(result, content);
    }

    #[test]
    fn before_validate_hooks_transform_html() {
        let lua = Lua::new();
        lua.load(
            r#"
            package.loaded["hooks"] = {
                upper = function(value, ctx)
                    if type(value) == "string" then
                        return value:upper()
                    end
                    return value
                end
            }
        "#,
        )
        .exec()
        .unwrap();

        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("tag", "Tag")
                .attrs(vec![
                    FieldDefinition::builder("label", FieldType::Text)
                        .hooks(FieldHooks {
                            before_validate: vec![HookRef::new("hooks.upper")],
                            ..Default::default()
                        })
                        .build(),
                ])
                .build(),
        );
        let field = formatted_field(vec!["tag".to_string()], "html");
        let content =
            r#"<p>Hi</p><crap-node data-type="tag" data-attrs='{"label":"hello"}'></crap-node>"#;

        let result = run_before_validate_on_node_attrs(&lua, content, &field, &reg, "pages");

        assert!(
            result.contains("HELLO"),
            "hook should uppercase the label: {result}"
        );
    }

    #[test]
    fn before_validate_html_escapes_single_quotes() {
        let lua = Lua::new();
        lua.load(
            r#"
            package.loaded["hooks"] = {
                add_quote = function(value, ctx)
                    if type(value) == "string" then
                        return value .. "'"
                    end
                    return value
                end
            }
        "#,
        )
        .exec()
        .unwrap();

        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("note", "Note")
                .attrs(vec![
                    FieldDefinition::builder("text", FieldType::Text)
                        .hooks(FieldHooks {
                            before_validate: vec![HookRef::new("hooks.add_quote")],
                            ..Default::default()
                        })
                        .build(),
                ])
                .build(),
        );
        let field = formatted_field(vec!["note".to_string()], "html");
        let content =
            r#"<p>Hi</p><crap-node data-type="note" data-attrs='{"text":"hello"}'></crap-node>"#;

        let result = run_before_validate_on_node_attrs(&lua, content, &field, &reg, "pages");

        // The single quote in the attr value must be escaped as &#39;
        assert!(
            result.contains("&#39;"),
            "single quote should be escaped: {result}"
        );
        assert!(
            !result.contains("data-attrs='{") || !result.contains("'}'"),
            "unescaped quote should not break the attribute boundary"
        );
    }
}
