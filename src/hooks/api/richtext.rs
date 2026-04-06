//! Registers `crap.richtext` — custom ProseMirror node registration and rendering.

use mlua::{Error::RuntimeError, Function, Lua, Table, Value};
use serde_json::Value as JsonValue;
use tracing::warn;

use super::parse::fields::parse_fields;
use crate::core::{
    FieldDefinition, SharedRegistry,
    richtext::{RichtextNodeDef, render_html_custom_nodes, render_prosemirror_to_html},
};

/// Validates that a node name is non-empty and contains only alphanumeric characters
/// and underscores.
fn validate_node_name(name: &str) -> mlua::Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err(RuntimeError(format!(
            "Invalid node name '{}': must be non-empty and contain only alphanumeric characters and underscores",
            name
        )));
    }

    Ok(())
}

/// Parses the `attrs` table from a node spec, validates that all types are scalar,
/// and warns on irrelevant features.
fn parse_node_attrs(name: &str, spec: &Table) -> mlua::Result<Vec<FieldDefinition>> {
    let attrs_tbl = match spec.get::<Table>("attrs") {
        Ok(tbl) => tbl,
        Err(_) => return Ok(Vec::new()),
    };

    let fields = parse_fields(&attrs_tbl)
        .map_err(|e| RuntimeError(format!("Invalid node attrs: {:#}", e)))?;

    for f in &fields {
        if !f.field_type.is_node_attr_type() {
            return Err(RuntimeError(format!(
                "Node attr '{}' has type '{}' which is not allowed as a node attribute. \
                 Allowed types: text, number, textarea, select, radio, checkbox, date, email, json, code",
                f.name,
                f.field_type.as_str(),
            )));
        }

        warn_irrelevant_node_attr_features(name, f);
    }

    Ok(fields)
}

/// Parses and validates `searchable_attrs` from a node spec, ensuring all referenced
/// attr names exist.
fn parse_searchable_attrs(
    name: &str,
    attrs: &[FieldDefinition],
    spec: &Table,
) -> mlua::Result<Vec<String>> {
    let searchable_attrs: Vec<String> = match spec.get::<Table>("searchable_attrs") {
        Ok(sa_tbl) => sa_tbl
            .sequence_values::<String>()
            .filter_map(|r| r.ok())
            .collect(),
        Err(_) => return Ok(Vec::new()),
    };

    let attr_names: Vec<&str> = attrs.iter().map(|a| a.name.as_str()).collect();

    for sa in &searchable_attrs {
        if !attr_names.contains(&sa.as_str()) {
            return Err(RuntimeError(format!(
                "Node '{}': searchable_attrs references unknown attr '{}'.\n\
                 Available attrs: [{}]",
                name,
                sa,
                attr_names.join(", "),
            )));
        }
    }

    Ok(searchable_attrs)
}

/// Stores a node entry (label, inline flag, optional render function) in the Lua registry.
fn store_node_in_lua(
    lua: &Lua,
    name: &str,
    label: &str,
    inline: bool,
    has_render: bool,
    spec: &Table,
) -> mlua::Result<()> {
    let storage: Table = lua.named_registry_value("_crap_richtext_nodes")?;

    let node_entry = lua.create_table()?;
    node_entry.set("label", label)?;
    node_entry.set("inline", inline)?;

    if has_render {
        let render_fn: Function = spec.get("render")?;
        node_entry.set("render", render_fn)?;
    }

    storage.set(name, node_entry)?;

    Ok(())
}

/// Handles the `crap.richtext.register_node(name, spec)` call — validates input,
/// parses attrs, and stores the node definition in both Lua and Rust registries.
fn register_node(
    lua: &Lua,
    registry: &SharedRegistry,
    name: String,
    spec: Table,
) -> mlua::Result<()> {
    validate_node_name(&name)?;

    let label: String = spec.get::<String>("label").unwrap_or_else(|_| name.clone());
    let inline: bool = spec.get::<bool>("inline").unwrap_or(false);
    let attrs = parse_node_attrs(&name, &spec)?;
    let searchable_attrs = parse_searchable_attrs(&name, &attrs, &spec)?;

    let has_render = spec
        .get::<Value>("render")
        .map(|v| matches!(v, Value::Function(_)))
        .unwrap_or(false);

    store_node_in_lua(lua, &name, &label, inline, has_render, &spec)?;

    let def = RichtextNodeDef::builder(&name, &label)
        .inline(inline)
        .attrs(attrs)
        .searchable_attrs(searchable_attrs)
        .has_render(has_render)
        .build();

    let mut reg = registry
        .write()
        .map_err(|e| RuntimeError(format!("Registry lock poisoned: {:#}", e)))?;
    reg.register_richtext_node(def);

    Ok(())
}

/// Renders richtext content (JSON or HTML) to HTML, invoking Lua render functions
/// for custom nodes.
fn render(lua: &Lua, content: String) -> mlua::Result<String> {
    let content = content.trim();

    if content.is_empty() {
        return Ok(String::new());
    }

    let storage: Table = lua.named_registry_value("_crap_richtext_nodes")?;

    let render_custom = |node_type: &str, attrs: &JsonValue| -> Option<String> {
        let entry: Table = storage.get(node_type).ok()?;
        let render_fn: Function = entry.get("render").ok()?;
        let attrs_lua = super::json_to_lua(lua, attrs).ok()?;

        match render_fn.call::<String>(attrs_lua) {
            Ok(html) => Some(html),
            Err(e) => {
                tracing::warn!("Render function for '{}' failed: {}", node_type, e);
                None
            }
        }
    };

    if content.starts_with('{') {
        render_prosemirror_to_html(content, &render_custom)
            .map_err(|e| RuntimeError(format!("Render error: {:#}", e)))
    } else {
        Ok(render_html_custom_nodes(content, &render_custom))
    }
}

/// Register the `crap.richtext` namespace on the `crap` global table.
///
/// Creates:
/// - `_crap_richtext_nodes` Lua global table (stores full specs including render functions)
/// - `crap.richtext.register_node(name, spec)` — registers a custom node type
/// - `crap.richtext.render(content_string)` — renders custom nodes to HTML
pub fn register_richtext(lua: &Lua, crap: &Table, registry: SharedRegistry) -> anyhow::Result<()> {
    let nodes_storage = lua.create_table()?;
    lua.set_named_registry_value("_crap_richtext_nodes", nodes_storage)?;

    let richtext_table = lua.create_table()?;

    let reg_clone = registry.clone();
    let register_node_fn = lua.create_function(move |lua, (name, spec): (String, Table)| {
        register_node(lua, &reg_clone, name, spec)
    })?;
    richtext_table.set("register_node", register_node_fn)?;

    let render_fn = lua.create_function(|lua, content: String| render(lua, content))?;
    richtext_table.set("render", render_fn)?;

    crap.set("richtext", richtext_table)?;

    Ok(())
}

/// Warn when a node attr uses features that have no effect on node attributes.
fn warn_irrelevant_node_attr_features(node_name: &str, f: &FieldDefinition) {
    let warn = |feature: &str| {
        warn!(
            "Node '{}' attr '{}': '{}' has no effect on node attributes",
            node_name, f.name, feature,
        );
    };

    // Hooks that don't apply (no per-attr write/read lifecycle)
    if !f.hooks.before_change.is_empty() {
        warn("hooks.before_change");
    }

    if !f.hooks.after_change.is_empty() {
        warn("hooks.after_change");
    }

    if !f.hooks.after_read.is_empty() {
        warn("hooks.after_read");
    }

    // Access control doesn't apply
    if f.access.read.is_some() {
        warn("access.read");
    }

    if f.access.create.is_some() {
        warn("access.create");
    }

    if f.access.update.is_some() {
        warn("access.update");
    }

    // DB features don't apply (no column)
    if f.unique {
        warn("unique");
    }

    if f.index {
        warn("index");
    }

    // Localized doesn't apply (richtext field itself is localized or not)
    if f.localized {
        warn("localized");
    }

    // has_many doesn't apply to scalar node attrs
    if f.has_many {
        warn("has_many");
    }

    // MCP description doesn't apply
    if f.mcp.description.is_some() {
        warn("mcp.description");
    }

    // admin.condition is deferred — warn for now
    if f.admin.condition.is_some() {
        warn("admin.condition");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Registry;
    use crate::hooks::api::fields::register_fields;

    fn setup_lua() -> (Lua, SharedRegistry) {
        let lua = Lua::new();
        let registry = Registry::shared();
        let crap = lua.create_table().unwrap();
        register_fields(&lua, &crap).unwrap();
        register_richtext(&lua, &crap, registry.clone()).unwrap();
        lua.globals().set("crap", crap).unwrap();
        (lua, registry)
    }

    #[test]
    fn register_node_basic() {
        let (lua, registry) = setup_lua();
        lua.load(
            r#"
            crap.richtext.register_node("cta", {
                label = "Call to Action",
                inline = false,
                attrs = {
                    crap.fields.text({ name = "text", required = true }),
                    crap.fields.text({ name = "url" }),
                },
                searchable_attrs = { "text" },
            })
        "#,
        )
        .exec()
        .unwrap();

        let reg = registry.read().unwrap();
        let node = reg.get_richtext_node("cta").unwrap();
        assert_eq!(node.label, "Call to Action");
        assert!(!node.inline);
        assert_eq!(node.attrs.len(), 2);
        assert!(node.attrs[0].required);
        assert!(!node.attrs[1].required);
        assert_eq!(node.searchable_attrs, vec!["text"]);
        assert!(!node.has_render);
    }

    #[test]
    fn register_node_with_render() {
        let (lua, registry) = setup_lua();
        lua.load(
            r#"
            crap.richtext.register_node("badge", {
                label = "Badge",
                inline = true,
                attrs = {
                    crap.fields.text({ name = "text", required = true }),
                },
                render = function(attrs)

                    return "<span class='badge'>" .. attrs.text .. "</span>"
                end,
            })
        "#,
        )
        .exec()
        .unwrap();

        let reg = registry.read().unwrap();
        let node = reg.get_richtext_node("badge").unwrap();
        assert!(node.inline);
        assert!(node.has_render);
    }

    #[test]
    fn register_node_invalid_name() {
        let (lua, _) = setup_lua();
        let result = lua
            .load(
                r#"
            crap.richtext.register_node("bad name!", { label = "Bad" })
        "#,
            )
            .exec();
        assert!(result.is_err());
    }

    #[test]
    fn render_json_with_custom_nodes() {
        let (lua, _) = setup_lua();
        lua.load(
            r#"
            crap.richtext.register_node("cta", {
                label = "CTA",
                render = function(attrs)

                    return '<a href="' .. attrs.url .. '">' .. attrs.text .. '</a>'
                end,
            })
        "#,
        )
        .exec()
        .unwrap();

        let result: String = lua.load(r#"

            return crap.richtext.render('{"type":"doc","content":[{"type":"cta","attrs":{"text":"Click","url":"/go"}}]}')
        "#).eval().unwrap();
        assert_eq!(result, r#"<a href="/go">Click</a>"#);
    }

    #[test]
    fn render_html_with_custom_nodes() {
        let (lua, _) = setup_lua();
        lua.load(
            r#"
            crap.richtext.register_node("cta", {
                label = "CTA",
                render = function(attrs)

                    return '<button>' .. attrs.text .. '</button>'
                end,
            })
        "#,
        )
        .exec()
        .unwrap();

        let result: String = lua.load(r#"

            return crap.richtext.render('<p>Hi</p><crap-node data-type="cta" data-attrs=\'{"text":"Go"}\'></crap-node>')
        "#).eval().unwrap();
        assert_eq!(result, "<p>Hi</p><button>Go</button>");
    }

    #[test]
    fn render_empty_string() {
        let (lua, _) = setup_lua();
        let result: String = lua
            .load(
                r#"

            return crap.richtext.render("")
        "#,
            )
            .eval()
            .unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn register_node_with_select_options() {
        let (lua, registry) = setup_lua();
        lua.load(
            r#"
            crap.richtext.register_node("alert", {
                label = "Alert",
                attrs = {
                    crap.fields.select({ name = "style", options = {
                        { label = "Info", value = "info" },
                        { label = "Warning", value = "warning" },
                    }}),
                },
            })
        "#,
        )
        .exec()
        .unwrap();

        let reg = registry.read().unwrap();
        let node = reg.get_richtext_node("alert").unwrap();
        assert_eq!(node.attrs[0].options.len(), 2);
    }

    #[test]
    fn register_node_empty_name_invalid() {
        let (lua, _) = setup_lua();
        let result = lua
            .load(
                r#"
            crap.richtext.register_node("", { label = "Empty" })
        "#,
            )
            .exec();
        assert!(result.is_err());
    }

    // render() with a registered node that has NO render function — should return
    // the <crap-node> passthrough (the `entry.get("render")` Err branch, line 107).
    #[test]
    fn render_json_node_without_render_function_passthrough() {
        let (lua, _) = setup_lua();
        // Register a node without a render function.
        lua.load(
            r#"
            crap.richtext.register_node("badge", {
                label = "Badge",
                attrs = {
                    crap.fields.text({ name = "text" }),
                },
            })
        "#,
        )
        .exec()
        .unwrap();

        // The render closure will find the node entry but no `render` key → return None
        // → rendered as <crap-node> passthrough.
        let result: String = lua.load(r#"

            return crap.richtext.render('{"type":"doc","content":[{"type":"badge","attrs":{"text":"hi"}}]}')
        "#).eval().unwrap();
        assert!(
            result.contains("crap-node"),
            "expected crap-node passthrough, got: {}",
            result
        );
        assert!(result.contains("data-type=\"badge\""));
    }

    // render() with a node type that was never registered at all — the
    // `storage.get(node_type)` Err branch (line 103) is exercised.
    #[test]
    fn render_json_unregistered_node_passthrough() {
        let (lua, _) = setup_lua();
        // No nodes registered at all.
        let result: String = lua.load(r#"

            return crap.richtext.render('{"type":"doc","content":[{"type":"mystery","attrs":{"x":"y"}}]}')
        "#).eval().unwrap();
        assert!(
            result.contains("crap-node"),
            "expected crap-node passthrough, got: {}",
            result
        );
        assert!(result.contains("data-type=\"mystery\""));
    }

    // render() where the Lua render function itself raises an error — the
    // `render_fn.call` Err branch (lines 116-119) is exercised.
    // The failing renderer returns None → passthrough <crap-node>.
    #[test]
    fn render_json_render_function_error_falls_back_to_passthrough() {
        let (lua, _) = setup_lua();
        lua.load(
            r#"
            crap.richtext.register_node("boom", {
                label = "Boom",
                render = function(attrs)
                    error("intentional render error")
                end,
            })
        "#,
        )
        .exec()
        .unwrap();

        let result: String = lua
            .load(
                r#"

            return crap.richtext.render('{"type":"doc","content":[{"type":"boom","attrs":{}}]}')
        "#,
            )
            .eval()
            .unwrap();
        // Render function failed → None → passthrough as <crap-node>
        assert!(
            result.contains("crap-node"),
            "expected crap-node passthrough, got: {}",
            result
        );
        assert!(result.contains("data-type=\"boom\""));
    }

    // render() with an invalid JSON string starting with '{' → RuntimeError
    #[test]
    fn render_invalid_json_returns_error() {
        let (lua, _) = setup_lua();
        let result = lua
            .load(
                r#"

            return crap.richtext.render("{not valid json")
        "#,
            )
            .exec();
        assert!(result.is_err());
    }

    #[test]
    fn register_node_rejects_non_scalar_attr_type() {
        let (lua, _) = setup_lua();
        let result = lua
            .load(
                r#"
            crap.richtext.register_node("bad", {
                label = "Bad",
                attrs = {
                    crap.fields.array({ name = "items" }),
                },
            })
        "#,
            )
            .exec();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not allowed"),
            "error should mention not allowed: {}",
            err_msg
        );
    }

    #[test]
    fn register_node_warns_on_irrelevant_features_but_succeeds() {
        let (lua, registry) = setup_lua();
        // Register with features that have no effect on node attrs.
        // Registration should succeed (warnings are logged, not errors).
        lua.load(
            r#"
            crap.richtext.register_node("warn_test", {
                label = "Warn Test",
                attrs = {
                    crap.fields.text({ name = "title", unique = true, index = true, localized = true }),
                },
            })
        "#,
        )
        .exec()
        .unwrap();

        let reg = registry.read().unwrap();
        let node = reg.get_richtext_node("warn_test").unwrap();
        assert_eq!(node.attrs.len(), 1);
        // The attrs still carry the original values — just warned
        assert!(node.attrs[0].unique);
        assert!(node.attrs[0].index);
        assert!(node.attrs[0].localized);
    }

    #[test]
    fn register_node_with_new_scalar_types() {
        let (lua, registry) = setup_lua();
        lua.load(
            r#"
            crap.richtext.register_node("form", {
                label = "Form",
                attrs = {
                    crap.fields.email({ name = "contact" }),
                    crap.fields.date({ name = "due_date" }),
                    crap.fields.radio({ name = "priority", options = {
                        { label = "Low", value = "low" },
                        { label = "High", value = "high" },
                    }}),
                    crap.fields.code({ name = "snippet" }),
                    crap.fields.json({ name = "metadata" }),
                    crap.fields.checkbox({ name = "active" }),
                    crap.fields.number({ name = "count" }),
                },
            })
        "#,
        )
        .exec()
        .unwrap();

        let reg = registry.read().unwrap();
        let node = reg.get_richtext_node("form").unwrap();
        assert_eq!(node.attrs.len(), 7);
    }

    #[test]
    fn register_node_searchable_attrs_unknown_rejected() {
        let (lua, _) = setup_lua();
        let result = lua
            .load(
                r#"
            crap.richtext.register_node("article", {
                label = "Article",
                attrs = {
                    crap.fields.text({ name = "title" }),
                },
                searchable_attrs = { "title", "nonexistent" },
            })
        "#,
            )
            .exec();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("nonexistent"),
            "error should mention the unknown attr: {}",
            err_msg
        );
    }
}
