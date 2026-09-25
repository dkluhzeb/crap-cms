//! `before_validate` hook pipeline for richtext node attrs. Walks `ProseMirror`
//! JSON or HTML and runs each node's per-attr `before_validate` Lua hooks,
//! transforming the attr values in-place.
//!
//! The hooks run exactly like field-level `before_validate` hooks — through the
//! same call ([`call_field_hook_ref`]), with the same context shape (`ctx.data`
//! is the node's attrs, `ctx.document` the whole document) — and fail closed
//! the same way: a hook that cannot be resolved, raises, or returns a value
//! that cannot be converted aborts the write.

use std::fmt::Write as _;

use anyhow::{Context as _, Result};
use mlua::Lua;
use serde_json::{Map, Value};

use crate::{
    core::{
        DocumentFields, FieldDefinition, FieldType, Registry, VisitAction, any_field,
        richtext::{CrapNodeTag, find_crap_nodes, parse_document, renderer::html_escape_attr},
        walk_nested_mut,
    },
    hooks::lifecycle::execution::{FieldHookMeta, call_field_hook_ref},
};

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
///
/// `meta` is the write's call metadata (collection, operation, id, locale),
/// handed to every hook in its context exactly as field hooks receive it.
///
/// # Errors
///
/// Returns the first hook failure — an unresolvable hook, a hook that raised,
/// or a return value that cannot be converted — naming the node and attr.
/// The write must not proceed with the attr value untransformed.
pub(crate) fn apply_node_attr_before_validate(
    lua: &Lua,
    fields: &[FieldDefinition],
    data: &mut DocumentFields,
    registry: &Registry,
    meta: &FieldHookMeta<'_>,
) -> Result<()> {
    if !has_node_attr_before_validate(fields, registry) {
        return Ok(());
    }

    let document = data.clone();
    let run = AttrHookRun {
        lua,
        registry,
        meta,
        document: &document,
    };
    let mut failure = None;

    walk_nested_mut(data, fields, &mut Vec::new(), &mut |field, level, _| {
        if failure.is_some() || !field_has_attr_hooks(field, registry) {
            return VisitAction::Keep;
        }

        let Some(value) = level.root_get(&field.name) else {
            return VisitAction::Keep;
        };

        match run.transform_value(field, value) {
            Ok(Some(new_value)) => VisitAction::Replace(new_value),
            Ok(None) => VisitAction::Keep,
            Err(e) => {
                failure = Some(e);
                VisitAction::Keep
            }
        }
    });

    failure.map_or(Ok(()), Err)
}

/// One pass of node-attr hooks over a write: the VM, the node definitions,
/// the write's call metadata and the document snapshot hooks see as
/// `ctx.document`.
struct AttrHookRun<'a> {
    lua: &'a Lua,
    registry: &'a Registry,
    meta: &'a FieldHookMeta<'a>,
    document: &'a DocumentFields,
}

impl AttrHookRun<'_> {
    /// One rich text value after its node-attr hooks, or `None` when
    /// unchanged. A JSON-format document may be its text or the document
    /// object; either keeps its shape. A value that is not a readable
    /// document is left for validation to refuse.
    fn transform_value(&self, field: &FieldDefinition, value: &Value) -> Result<Option<Value>> {
        match value {
            Value::String(content) => {
                let new_content = self.transform_text(field, content)?;
                Ok((new_content != *content).then_some(Value::String(new_content)))
            }
            Value::Object(_) if field.parses_json() => {
                if parse_document(value).is_none() {
                    return Ok(None);
                }

                let mut doc = value.clone();
                let modified = self.transform_json(field, &mut doc)?;

                Ok(modified.then_some(doc))
            }
            _ => Ok(None),
        }
    }

    /// One rich text content string (`ProseMirror` JSON text or HTML, per the
    /// field's format) after its node-attr hooks.
    fn transform_text(&self, field: &FieldDefinition, content: &str) -> Result<String> {
        if !field.parses_json() {
            return self.transform_html(field, content);
        }

        let Ok(mut parsed) = serde_json::from_str::<Value>(content) else {
            return Ok(content.to_string());
        };

        if !self.transform_json(field, &mut parsed)? {
            return Ok(content.to_string());
        }

        Ok(serde_json::to_string(&parsed)?)
    }

    /// Run the hooks on every declared node in a `ProseMirror` JSON tree;
    /// `true` when an attr changed.
    fn transform_json(&self, field: &FieldDefinition, value: &mut Value) -> Result<bool> {
        let node_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let mut modified = false;

        if let Some(attrs) = value.get_mut("attrs").and_then(Value::as_object_mut) {
            modified |= self.transform_attrs(field, &node_type, attrs)?;
        }

        if let Some(content) = value.get_mut("content").and_then(Value::as_array_mut) {
            for child in content {
                modified |= self.transform_json(field, child)?;
            }
        }

        Ok(modified)
    }

    /// Run the hooks on HTML content. Nodes are found with the tokenizer
    /// validation and the renderer use; a node whose attrs a hook changed is
    /// rewritten, everything else is copied verbatim.
    fn transform_html(&self, field: &FieldDefinition, content: &str) -> Result<String> {
        let mut result = String::with_capacity(content.len());
        let mut copied = 0;

        for tag in find_crap_nodes(content) {
            let Some(rewritten) = self.rewrite_html_node(field, &tag)? else {
                continue;
            };

            result.push_str(&content[copied..tag.start]);
            result.push_str(&rewritten);
            copied = tag.end;
        }

        result.push_str(&content[copied..]);
        Ok(result)
    }

    /// The node's markup after its attr hooks ran, or `None` when the node
    /// has no hooks or they changed nothing.
    fn rewrite_html_node(
        &self,
        field: &FieldDefinition,
        tag: &CrapNodeTag,
    ) -> Result<Option<String>> {
        let Some(node_type) = tag.node_type() else {
            return Ok(None);
        };

        let mut attrs = tag.node_attrs();

        if !self.transform_attrs(field, node_type, &mut attrs)? {
            return Ok(None);
        }

        let mut out = String::new();
        let _ = write!(
            out,
            "<crap-node data-type=\"{}\" data-attrs='{}'></crap-node>",
            html_escape_attr(node_type),
            html_escape_attr(&serde_json::to_string(&attrs)?),
        );

        Ok(Some(out))
    }

    /// Run each hooked attr's `before_validate` chain on one node instance of
    /// a node the field declares; `true` when an attr changed.
    fn transform_attrs(
        &self,
        field: &FieldDefinition,
        node_type: &str,
        attrs: &mut Map<String, Value>,
    ) -> Result<bool> {
        if !field.admin.nodes.iter().any(|n| n == node_type) {
            return Ok(false);
        }

        let Some(node_def) = self.registry.get_richtext_node(node_type) else {
            return Ok(false);
        };

        let mut changed = false;

        for attr_def in &node_def.attrs {
            let Some(new_val) = self.run_attr_hooks(node_type, attr_def, attrs)? else {
                continue;
            };

            attrs.insert(attr_def.name.clone(), new_val);
            changed = true;
        }

        Ok(changed)
    }

    /// Run one attr's `before_validate` chain over its current value; the new
    /// value, or `None` when the attr is absent, unhooked or unchanged.
    fn run_attr_hooks(
        &self,
        node_type: &str,
        attr_def: &FieldDefinition,
        attrs: &Map<String, Value>,
    ) -> Result<Option<Value>> {
        let hooks = &attr_def.hooks.before_validate;

        let Some(original) = attrs.get(&attr_def.name).filter(|_| !hooks.is_empty()) else {
            return Ok(None);
        };

        let scope: DocumentFields = attrs.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let mut current = original.clone();

        for hook in hooks {
            current = call_field_hook_ref(
                self.lua,
                hook,
                &current,
                &attr_def.name,
                self.meta,
                &scope,
                self.document,
            )
            .with_context(|| {
                format!(
                    "before_validate hook '{}' for node '{node_type}' attr '{}' failed",
                    hook.reference(),
                    attr_def.name
                )
            })?;
        }

        Ok((current != *original).then_some(current))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::core::{
        FieldHooks, HookRef, RichtextNodeDef,
        field::{FieldAdmin, FieldType},
    };

    use super::*;

    const META: FieldHookMeta<'static> = FieldHookMeta {
        collection: "pages",
        operation: "update",
        id: Some("doc-1"),
        locale: Some("en"),
    };

    /// One content string through the node-attr hooks of `field`.
    fn run_text(lua: &Lua, content: &str, field: &FieldDefinition, registry: &Registry) -> String {
        let document = DocumentFields::new();
        let run = AttrHookRun {
            lua,
            registry,
            meta: &META,
            document: &document,
        };

        run.transform_text(field, content).unwrap()
    }

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

    /// With no custom nodes, the short-circuit leaves the value verbatim —
    /// crucially WITHOUT parsing it, so even invalid JSON/HTML passes through
    /// untouched (and no Lua runs).
    #[test]
    fn no_custom_nodes_returns_content_verbatim() {
        let lua = Lua::new();
        let registry = Registry::new();
        let fields = vec![richtext_field(vec![])];

        let garbage = json!("{ this is not valid json <crap-node");
        let mut data: DocumentFields = [("body".to_string(), garbage.clone())]
            .into_iter()
            .collect();

        apply_node_attr_before_validate(&lua, &fields, &mut data, &registry, &META).unwrap();
        assert_eq!(data["body"], garbage);
    }

    /// A node name is declared on the field but the registry has no matching
    /// node definition (hence no `before_validate` hooks) → still a pass-through.
    #[test]
    fn declared_node_absent_from_registry_is_pass_through() {
        let lua = Lua::new();
        let registry = Registry::new();
        let fields = vec![richtext_field(vec!["callout".into()])];

        let content = json!(r#"{"type":"doc","content":[]}"#);
        let mut data: DocumentFields = [("body".to_string(), content.clone())]
            .into_iter()
            .collect();

        apply_node_attr_before_validate(&lua, &fields, &mut data, &registry, &META).unwrap();
        assert_eq!(data["body"], content);
    }

    /// A registry whose `note` node's `text` attr runs `hook` before
    /// validation.
    fn hooked_note_registry(hook: &str) -> Registry {
        let mut registry = Registry::new();
        registry.register_richtext_node(
            RichtextNodeDef::builder("note", "Note")
                .attrs(vec![
                    FieldDefinition::builder("text", FieldType::Text)
                        .hooks(FieldHooks {
                            before_validate: vec![HookRef::new(hook)],
                            ..Default::default()
                        })
                        .build(),
                ])
                .build(),
        );
        registry
    }

    fn note_data() -> DocumentFields {
        [("body".to_string(), note_doc(" hi "))]
            .into_iter()
            .collect()
    }

    /// Regression: a node-attr hook that could not be found was skipped with a
    /// warning and the raw value stored — unlike a field-level hook, which
    /// aborts the write.
    #[test]
    fn a_missing_hook_fails_the_write() {
        let lua = Lua::new();
        let registry = hooked_note_registry("hooks.missing");
        let mut data = note_data();

        let err = apply_node_attr_before_validate(
            &lua,
            &[note_field("body")],
            &mut data,
            &registry,
            &META,
        )
        .unwrap_err();

        assert!(format!("{err:#}").contains("hooks.missing"), "{err:#}");
        assert_eq!(data["body"], note_doc(" hi "), "the value is left as sent");
    }

    /// Regression: a hook that raised was logged and its value kept unchanged.
    #[test]
    fn a_raising_hook_fails_the_write() {
        let lua = Lua::new();
        lua.load(r#"package.loaded["hooks"] = { boom = function() error("nope") end }"#)
            .exec()
            .unwrap();
        let registry = hooked_note_registry("hooks.boom");

        let err = apply_node_attr_before_validate(
            &lua,
            &[note_field("body")],
            &mut note_data(),
            &registry,
            &META,
        )
        .unwrap_err();

        let msg = format!("{err:#}");
        assert!(
            msg.contains("note") && msg.contains("text") && msg.contains("nope"),
            "{msg}"
        );
    }

    /// A node-attr hook gets the field-hook context: the write's operation,
    /// collection, id and locale, the attr as `field_name`, the node's attrs
    /// as `data` and the whole document as `document`.
    #[test]
    fn hooks_receive_the_field_hook_context() {
        let lua = Lua::new();
        lua.load(
            r#"package.loaded["hooks"] = { describe = function(value, ctx)
                return table.concat({
                    ctx.operation, ctx.collection, ctx.id, ctx.locale, ctx.field_name,
                    ctx.data.text, type(ctx.document.body),
                }, "|")
            end }"#,
        )
        .exec()
        .unwrap();
        let registry = hooked_note_registry("hooks.describe");
        let mut data = note_data();

        apply_node_attr_before_validate(&lua, &[note_field("body")], &mut data, &registry, &META)
            .unwrap();

        assert_eq!(
            data["body"],
            note_doc("update|pages|doc-1|en|text| hi |table")
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

        apply_node_attr_before_validate(&lua, &fields, &mut data, &registry, &META).unwrap();

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

        apply_node_attr_before_validate(&lua, &fields, &mut data, &registry, &META).unwrap();

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

        let result = run_text(&lua, content, &field, &reg);

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

        let result = run_text(&lua, content, &field, &reg);
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

        let result = run_text(&lua, content, &field, &reg);

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

        let result = run_text(&lua, content, &field, &reg);

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
