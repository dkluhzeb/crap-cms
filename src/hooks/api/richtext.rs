//! Registers `crap.richtext` — custom ProseMirror node registration and rendering.

use anyhow::Result;
use mlua::{Function, Lua, Table, Value};

use crate::core::SharedRegistry;
use crate::core::field::LocalizedString;
use crate::core::field::SelectOption;
use crate::core::richtext::{NodeAttr, NodeAttrType, RichtextNodeDef};

/// Register the `crap.richtext` namespace on the `crap` global table.
///
/// Creates:
/// - `_crap_richtext_nodes` Lua global table (stores full specs including render functions)
/// - `crap.richtext.register_node(name, spec)` — registers a custom node type
/// - `crap.richtext.render(content_string)` — renders custom nodes to HTML
pub fn register_richtext(lua: &Lua, crap: &Table, registry: SharedRegistry) -> Result<()> {
    // Lua-side storage for node specs (including render functions)
    let nodes_storage = lua.create_table()?;
    lua.globals().set("_crap_richtext_nodes", nodes_storage)?;

    let richtext_table = lua.create_table()?;

    // crap.richtext.register_node(name, spec)
    let reg_clone = registry.clone();
    let register_node_fn = lua.create_function(move |lua, (name, spec): (String, Table)| {
        // Validate name: alphanumeric + underscore
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(mlua::Error::RuntimeError(format!(
                "Invalid node name '{}': must be non-empty and contain only alphanumeric characters and underscores",
                name
            )));
        }

        let label: String = spec.get::<String>("label")
            .unwrap_or_else(|_| name.clone());
        let inline: bool = spec.get::<bool>("inline").unwrap_or(false);

        // Parse attrs
        let attrs = if let Ok(attrs_tbl) = spec.get::<Table>("attrs") {
            parse_node_attrs(lua, &attrs_tbl)?
        } else {
            Vec::new()
        };

        // Parse searchable_attrs
        let searchable_attrs = if let Ok(sa_tbl) = spec.get::<Table>("searchable_attrs") {
            sa_tbl.sequence_values::<String>()
                .filter_map(|r| r.ok())
                .collect()
        } else {
            Vec::new()
        };

        // Check for render function
        let has_render = spec.get::<Value>("render")
            .map(|v| matches!(v, Value::Function(_)))
            .unwrap_or(false);

        // Store the full spec in Lua globals (including render function)
        let globals = lua.globals();
        let storage: Table = globals.get("_crap_richtext_nodes")?;
        let node_entry = lua.create_table()?;
        node_entry.set("label", label.as_str())?;
        node_entry.set("inline", inline)?;
        if has_render {
            let render_fn: Function = spec.get("render")?;
            node_entry.set("render", render_fn)?;
        }
        storage.set(name.as_str(), node_entry)?;

        // Register in Rust registry
        let def = RichtextNodeDef::builder(&name, &label)
            .inline(inline)
            .attrs(attrs)
            .searchable_attrs(searchable_attrs)
            .has_render(has_render)
            .build();
        let mut reg = reg_clone.write()
            .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock poisoned: {}", e)))?;
        reg.register_richtext_node(def);

        Ok(())
    })?;
    richtext_table.set("register_node", register_node_fn)?;

    // crap.richtext.render(content_string)
    let render_fn = lua.create_function(|lua, content: String| -> mlua::Result<String> {
        let content = content.trim();
        if content.is_empty() {
            return Ok(String::new());
        }

        let globals = lua.globals();
        let storage: Table = globals.get("_crap_richtext_nodes")?;

        // Build the custom renderer closure that calls Lua render functions
        let render_custom = |node_type: &str, attrs: &serde_json::Value| -> Option<String> {
            let entry: Table = match storage.get(node_type) {
                Ok(e) => e,
                Err(_) => return None,
            };
            let render_fn: Function = match entry.get("render") {
                Ok(f) => f,
                Err(_) => return None,
            };
            // Convert attrs JSON to Lua table
            let attrs_lua = match super::json_to_lua(lua, attrs) {
                Ok(v) => v,
                Err(_) => return None,
            };
            match render_fn.call::<String>(attrs_lua) {
                Ok(html) => Some(html),
                Err(e) => {
                    tracing::warn!("Render function for '{}' failed: {}", node_type, e);
                    None
                }
            }
        };

        // Detect format: starts with '{' → JSON, otherwise HTML
        if content.starts_with('{') {
            crate::core::richtext::render_prosemirror_to_html(content, &render_custom)
                .map_err(|e| mlua::Error::RuntimeError(format!("Render error: {}", e)))
        } else {
            Ok(crate::core::richtext::render_html_custom_nodes(
                content,
                &render_custom,
            ))
        }
    })?;
    richtext_table.set("render", render_fn)?;

    crap.set("richtext", richtext_table)?;
    Ok(())
}

/// Parse a Lua array of attr tables into `Vec<NodeAttr>`.
fn parse_node_attrs(lua: &Lua, attrs_tbl: &Table) -> mlua::Result<Vec<NodeAttr>> {
    let mut attrs = Vec::new();
    for pair in attrs_tbl.clone().sequence_values::<Table>() {
        let attr_tbl = pair?;
        let name: String = attr_tbl.get("name")?;
        let attr_type_str: String = attr_tbl
            .get::<String>("type")
            .unwrap_or_else(|_| "text".into());
        let label: String = attr_tbl
            .get::<String>("label")
            .unwrap_or_else(|_| name.clone());
        let required: bool = attr_tbl.get::<bool>("required").unwrap_or(false);

        let default_value = match attr_tbl.get::<Value>("default")? {
            Value::Nil => None,
            v => Some(super::lua_to_json(lua, &v)?),
        };

        let options = if let Ok(opts_tbl) = attr_tbl.get::<Table>("options") {
            parse_select_options(&opts_tbl)?
        } else {
            Vec::new()
        };

        let mut node_attr_builder = NodeAttr::builder(name, label)
            .attr_type(NodeAttrType::from_name(&attr_type_str))
            .required(required)
            .options(options);
        if let Some(dv) = default_value {
            node_attr_builder = node_attr_builder.default_value(dv);
        }
        attrs.push(node_attr_builder.build());
    }
    Ok(attrs)
}

/// Parse select options from a Lua table.
fn parse_select_options(tbl: &Table) -> mlua::Result<Vec<SelectOption>> {
    let mut options = Vec::new();
    for pair in tbl.clone().sequence_values::<Table>() {
        let opt_tbl = pair?;
        let label: String = opt_tbl.get("label")?;
        let value: String = opt_tbl.get("value")?;
        options.push(SelectOption::new(LocalizedString::Plain(label), value));
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Registry;

    fn setup_lua() -> (Lua, SharedRegistry) {
        let lua = Lua::new();
        let registry = Registry::shared();
        let crap = lua.create_table().unwrap();
        register_richtext(&lua, &crap, registry.clone()).unwrap();
        lua.globals().set("crap", crap).unwrap();
        // Also register json_to_lua helper for round-tripping
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
                    { name = "text", type = "text", label = "Button Text", required = true },
                    { name = "url", type = "text", label = "URL" },
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
                    { name = "text", type = "text", label = "Text", required = true },
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
                    { name = "style", type = "select", label = "Style", options = {
                        { label = "Info", value = "info" },
                        { label = "Warning", value = "warning" },
                    }},
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
                attrs = { { name = "text", type = "text" } },
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
}
