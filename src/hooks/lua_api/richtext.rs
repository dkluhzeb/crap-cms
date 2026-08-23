//! Registers `crap.richtext` — custom `ProseMirror` node registration and rendering.

use mlua::{Error::RuntimeError, FromLua, Function, Lua, Result as LuaResult, Table, Value};
use serde_json::Value as JsonValue;
use tracing::warn;

use super::parse::{deny_unknown_keys, fields::parse_fields, get_bool, get_string_strict};
use super::utils::{lua_err, registry_lock_poisoned, require_init_phase};
use crate::core::{
    FieldDefinition, RichtextNodeDef, SharedRegistry,
    richtext::{render_html_custom_nodes, render_prosemirror_to_html},
};
use crate::typegen::lua::{LuaAnnotation, LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Spec for registering a custom richtext node. Parsed from the Lua
/// table the user passes to `crap.richtext.register_node(name, spec)`.
///
/// `attrs` is the only Lua-typed field that's converted to a Rust
/// representation eagerly (via `parse_fields`) so the call site can
/// run the type-allow-list validation against typed
/// `FieldDefinition`s rather than re-walking a Lua table. `render`
/// stays as `mlua::Function` because it's invoked from Rust later, and
/// `mlua::Function` is the natural type for that.
#[derive(LuaAnnotation)]
#[lua(class = "crap.RichtextNodeSpec")]
pub(crate) struct RichtextNodeSpec {
    /// Display label (defaults to `name`).
    #[lua(optional)]
    pub(crate) label: Option<String>,
    /// Whether the node is inline (default: `false` = block).
    #[lua(optional)]
    pub(crate) inline: bool,
    /// Attribute definitions (scalar types only: text, number, textarea,
    /// select, radio, checkbox, date, email, json, code). Use
    /// `crap.fields.*` factory functions.
    #[lua(ty = "crap.FieldDefinition[]", optional)]
    pub(crate) attrs: Vec<FieldDefinition>,
    /// Attr names to include in FTS search index.
    #[lua(optional)]
    pub(crate) searchable_attrs: Vec<String>,
    /// Server-side render function. Receives the node attrs as a Lua
    /// table; returns the rendered HTML string.
    #[lua(ty = "fun(attrs: table): string", optional)]
    pub(crate) render: Option<Function>,
}

impl FromLua for RichtextNodeSpec {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        let Value::Table(tbl) = value else {
            return Err(RuntimeError(format!(
                "crap.richtext.register_node spec must be a table, got {}",
                value.type_name()
            )));
        };

        deny_unknown_keys(
            &tbl,
            "crap.richtext.register_node spec",
            &["label", "inline", "attrs", "searchable_attrs", "render"],
        )
        .map_err(lua_err)?;

        let attrs = match tbl.get::<Option<Table>>("attrs")? {
            Some(attrs_tbl) => parse_fields(lua, &attrs_tbl)
                .map_err(|e| RuntimeError(format!("Invalid node attrs: {e:#}")))?,
            None => Vec::new(),
        };

        let searchable_attrs = match tbl.get::<Option<Table>>("searchable_attrs")? {
            Some(sa_tbl) => sa_tbl
                .sequence_values::<String>()
                .collect::<LuaResult<Vec<_>>>()
                .map_err(|e| {
                    RuntimeError(format!(
                        "crap.richtext.register_node: `searchable_attrs` must be \
                         an array of strings: {e}"
                    ))
                })?,
            None => Vec::new(),
        };

        // Strict reads: `tbl.get::<Option<bool>>` applies Lua truthiness to
        // ANY value (so `inline = "false"` would register as inline TRUE),
        // and `Option<String>` coerces silently. Use the strict helpers so a
        // wrong-typed value errors at load, matching the project invariant.
        Ok(Self {
            label: get_string_strict(&tbl, "label", "crap.richtext.register_node spec")?,
            inline: get_bool(&tbl, "inline", false)?,
            attrs,
            searchable_attrs,
            render: tbl.get::<Option<Function>>("render")?,
        })
    }
}

/// Built-in `ProseMirror` node types. Registering a custom node with one
/// of these names would silently fail at render time — the built-in
/// match arm in `core::richtext::renderer::render_node` runs first and
/// the custom renderer is never called. Reject the registration so the
/// plugin author sees the conflict immediately.
const RESERVED_NODE_NAMES: &[&str] = &[
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

/// Validates that a node name is non-empty, contains only lowercase ASCII
/// letters, digits, and underscores, does not start with a digit or
/// underscore, and does not collide with a built-in `ProseMirror` node type
/// (the built-in match arm in the renderer would shadow the custom render
/// function). The charset matches every other identifier in the system
/// (`validate_slug`) — `is_alphanumeric` used to accept Unicode/uppercase.
fn validate_node_name(name: &str) -> LuaResult<()> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit() || c == '_');

    if !valid {
        return Err(RuntimeError(format!(
            "Invalid node name '{name}': must be non-empty, use only lowercase ASCII letters, \
             digits, and underscores, and not start with a digit or underscore"
        )));
    }

    if RESERVED_NODE_NAMES.contains(&name) {
        return Err(RuntimeError(format!(
            "Invalid node name '{name}': collides with a built-in ProseMirror node type. \
             Built-in names {RESERVED_NODE_NAMES:?} are reserved — pick a different name."
        )));
    }

    Ok(())
}

/// Validate that every parsed attr uses a scalar type and warn about
/// features (validate, hooks, …) that don't fire for node attrs.
fn validate_node_attrs(name: &str, attrs: &[FieldDefinition]) -> LuaResult<()> {
    for f in attrs {
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
    Ok(())
}

/// Validate every entry in `searchable_attrs` references a real attr.
fn validate_searchable_attrs(
    name: &str,
    attrs: &[FieldDefinition],
    searchable_attrs: &[String],
) -> LuaResult<()> {
    let attr_names: Vec<&str> = attrs.iter().map(|a| a.name.as_str()).collect();

    for sa in searchable_attrs {
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

    Ok(())
}

/// Stores a node entry (label, inline flag, optional render function) in the Lua registry.
fn store_node_in_lua(
    lua: &Lua,
    name: &str,
    label: &str,
    inline: bool,
    render: Option<&Function>,
) -> LuaResult<()> {
    let storage: Table = lua.named_registry_value("_crap_richtext_nodes")?;

    let node_entry = lua.create_table()?;
    node_entry.set("label", label)?;
    node_entry.set("inline", inline)?;

    if let Some(render_fn) = render {
        node_entry.set("render", render_fn.clone())?;
    }

    storage.set(name, node_entry)?;

    Ok(())
}

/// Handles the `crap.richtext.register_node(name, spec)` call — validates input,
/// and stores the node definition in both Lua and Rust registries. Attr
/// parsing already happened in `RichtextNodeSpec::from_lua`.
fn register_node(
    lua: &Lua,
    registry: &SharedRegistry,
    name: &str,
    spec: RichtextNodeSpec,
) -> LuaResult<()> {
    // Custom nodes must be registered at init time so all VMs in the
    // pool share the same node set and the per-collection field-context
    // builder sees them consistently. A runtime call from a hook would
    // only land in the current VM and fragment across the pool.
    require_init_phase(lua, REGISTER_NODE_INIT_ONLY_ERROR)?;

    validate_node_name(name)?;
    validate_node_attrs(name, &spec.attrs)?;
    validate_searchable_attrs(name, &spec.attrs, &spec.searchable_attrs)?;

    let label = spec.label.unwrap_or_else(|| name.to_string());
    let has_render = spec.render.is_some();

    store_node_in_lua(lua, name, &label, spec.inline, spec.render.as_ref())?;

    let def = RichtextNodeDef::builder(name, &label)
        .inline(spec.inline)
        .attrs(spec.attrs)
        .searchable_attrs(spec.searchable_attrs)
        .has_render(has_render)
        .build();

    let mut reg = registry.write().map_err(registry_lock_poisoned)?;
    reg.register_richtext_node(def);

    Ok(())
}

/// Renders richtext content (JSON or HTML) to HTML, invoking Lua render functions
/// for custom nodes.
fn render(lua: &Lua, content: &str) -> LuaResult<String> {
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
                warn!("Render function for '{}' failed: {}", node_type, e);
                None
            }
        }
    };

    if content.starts_with('{') {
        render_prosemirror_to_html(content, &render_custom)
            .map_err(|e| RuntimeError(format!("Render error: {e:#}")))
    } else {
        Ok(render_html_custom_nodes(content, &render_custom))
    }
}

const REGISTER_NODE_INIT_ONLY_ERROR: &str = "crap.richtext.register_node must be called from \
     init.lua or a definition file — runtime registration only lands in one VM of the pool";

// ── User-facing fns ──────────────────────────────────────────────────

/// Register a custom `ProseMirror` node type.
#[lua_fn(path = "crap.richtext.register_node")]
fn richtext_register_node_init(
    state: &SharedRegistry,
    lua: &Lua,
    #[lua(doc = "Node name (alphanumeric + underscores only).")] name: String,
    #[lua(ty = "crap.RichtextNodeSpec", doc = "Node specification.")] spec: RichtextNodeSpec,
) -> LuaResult<()> {
    register_node(lua, state, &name, spec)
}

/// Pool-VM variant of `register_node`. Same `InitPhase` guard, does the
/// per-VM Lua-side storage so `render` can find the node's render
/// function, but skips the shared-registry write — the `init_lua` VM
/// already populated the registry.
#[lua_fn(path = "crap.richtext.register_node")]
fn richtext_register_node_pool(
    _state: &(),
    lua: &Lua,
    name: String,
    #[lua(ty = "crap.RichtextNodeSpec")] spec: RichtextNodeSpec,
) -> LuaResult<()> {
    register_node_pool(lua, &name, spec)
}

/// Render richtext content, replacing custom nodes with their rendered HTML.
/// Detects format automatically: starts with '{' = JSON, otherwise HTML.
#[lua_fn(path = "crap.richtext.render", returns_doc = "Rendered HTML output.")]
fn richtext_render(
    lua: &Lua,
    #[lua(doc = "Richtext content (HTML or `ProseMirror` JSON).")] content: String,
) -> LuaResult<String> {
    render(lua, &content)
}

lua_table! {
    name: crap_richtext_init,
    path: "crap.richtext",
    state: SharedRegistry,
    header: "Custom ProseMirror node registration and rendering.",
    fns: [richtext_register_node_init],
}

lua_table! {
    name: crap_richtext_pool,
    path: "crap.richtext",
    state: (),
    fns: [richtext_register_node_pool],
}

// `render` is stateless and shared by both init and pool entry points.
lua_table! {
    name: crap_richtext_render,
    path: "crap.richtext",
    state: (),
    fns: [richtext_render],
}

// ── Registration entry points ────────────────────────────────────────

/// Init-time registration of `crap.richtext`: write-capable
/// `register_node` + `render`. Used by the init-phase Lua VM.
pub fn register_richtext_init(lua: &Lua, registry: SharedRegistry) -> anyhow::Result<()> {
    let nodes_storage = lua.create_table()?;
    lua.set_named_registry_value("_crap_richtext_nodes", nodes_storage)?;
    register_crap_richtext_init(lua, registry)?;
    register_crap_richtext_render(lua, ())?;
    Ok(())
}

/// Pool-VM registration of `crap.richtext`: `register_node` does the
/// per-VM Lua-side storage (which `render` needs to find the node's
/// render function) but skips the shared-registry write — the
/// `init_lua` VM already populated the registry. Pool VMs run
/// `init.lua` (and anything it requires), so any
/// `crap.richtext.register_node(...)` calls hit this path with
/// `InitPhase` set, populating the per-VM Lua-side table.
pub fn register_richtext_pool_init(
    lua: &Lua,
    _registry: std::sync::Arc<crate::core::Registry>,
) -> anyhow::Result<()> {
    let nodes_storage = lua.create_table()?;
    lua.set_named_registry_value("_crap_richtext_nodes", nodes_storage)?;
    register_crap_richtext_pool(lua, ())?;
    register_crap_richtext_render(lua, ())?;
    Ok(())
}

/// Pool-VM `register_node`: validates + stores the node entry in the
/// VM's named-registry table (so `render` can find it), skips the
/// shared-registry write. The shared registry was already populated
/// by the `init_lua` VM.
fn register_node_pool(lua: &Lua, name: &str, spec: RichtextNodeSpec) -> LuaResult<()> {
    require_init_phase(lua, REGISTER_NODE_INIT_ONLY_ERROR)?;

    validate_node_name(name)?;
    // Validate attrs here too — errors surface to the user even though
    // the shared registry already has the canonical `RichtextNodeDef`.
    validate_node_attrs(name, &spec.attrs)?;

    let label = spec.label.unwrap_or_else(|| name.to_string());

    store_node_in_lua(lua, name, &label, spec.inline, spec.render.as_ref())?;

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
    use std::sync::Arc;

    use super::*;
    use crate::core::Registry;
    use crate::hooks::lifecycle::InitPhase;
    use crate::hooks::lua_api::fields::register_fields;

    fn setup_lua() -> (Lua, SharedRegistry) {
        let lua = Lua::new();
        let registry = Registry::shared();
        let crap = lua.create_table().unwrap();
        lua.globals().set("crap", crap.clone()).unwrap();
        register_fields(&lua).unwrap();
        register_richtext_init(&lua, Arc::clone(&registry)).unwrap();
        // Mimic init-time loading so register_node accepts the call.
        lua.set_app_data(InitPhase);
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

    /// Regression: an unknown spec key (e.g. a typo'd `label`) must be
    /// rejected at load time, not silently dropped — parity with every other
    /// strict Lua schema table.
    #[test]
    fn register_node_unknown_spec_key_rejected() {
        let (lua, _registry) = setup_lua();

        let err = lua
            .load(r#"crap.richtext.register_node("cta", { lable = "CTA" })"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("lable") && err.contains("label"),
            "expected unknown-key error with suggestion, got: {err}"
        );
    }

    /// Regression: a non-string `searchable_attrs` entry must hard-error
    /// instead of being silently dropped from the FTS index.
    #[test]
    fn register_node_non_string_searchable_attr_rejected() {
        let (lua, _registry) = setup_lua();

        let err = lua
            .load(
                r#"
                crap.richtext.register_node("cta", {
                    attrs = { crap.fields.text({ name = "text" }) },
                    searchable_attrs = { "text", true },
                })
            "#,
            )
            .exec()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("searchable_attrs") && err.contains("array of strings"),
            "expected searchable_attrs type error, got: {err}"
        );
    }

    /// Regression: `inline` was read via mlua's `Option<bool>` conversion,
    /// which applies Lua truthiness to ANY value — `inline = "false"` (a
    /// truthy string) silently registered the node as inline TRUE. A
    /// wrong-typed value must now error.
    #[test]
    fn register_node_wrong_typed_inline_rejected() {
        let (lua, _registry) = setup_lua();

        let err = lua
            .load(r#"crap.richtext.register_node("cta", { inline = "false" })"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("inline") && err.contains("boolean"),
            "expected a boolean type error for inline, got: {err}"
        );
    }

    /// Regression: `validate_node_name` used the Unicode-aware
    /// `is_alphanumeric`, accepting uppercase, non-ASCII, and leading
    /// digits/underscores — diverging from every other identifier rule.
    #[test]
    fn register_node_name_must_be_ascii_slug() {
        for bad in ["CTA", "日本語", "_x", "9x", "my-node"] {
            let (lua, _registry) = setup_lua();
            let code = format!(r#"crap.richtext.register_node("{bad}", {{}})"#);
            let err = lua.load(&code).exec().unwrap_err().to_string();
            assert!(
                err.contains("Invalid node name"),
                "node name '{bad}' should be rejected, got: {err}"
            );
        }

        // A valid lowercase-ASCII name still works.
        let (lua, registry) = setup_lua();
        lua.load(r#"crap.richtext.register_node("my_node", {})"#)
            .exec()
            .unwrap();
        assert!(
            registry
                .read()
                .unwrap()
                .get_richtext_node("my_node")
                .is_some()
        );
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
            "expected crap-node passthrough, got: {result}"
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
            "expected crap-node passthrough, got: {result}"
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
            "expected crap-node passthrough, got: {result}"
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
            "error should mention not allowed: {err_msg}"
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
            "error should mention the unknown attr: {err_msg}"
        );
    }

    /// Regression: registering a custom node whose name collides with a
    /// built-in `ProseMirror` type (e.g. `paragraph`, `heading`) used to
    /// succeed silently — the renderer's match arm hits the built-in
    /// branch first and the custom render function is never called, so
    /// the plugin author sees their custom widget mysteriously fail to
    /// appear. Now reject at registration time.
    #[test]
    fn register_node_rejects_builtin_name_collision() {
        let (lua, _registry) = setup_lua();
        let result = lua
            .load(r#"crap.richtext.register_node("paragraph", { label = "Custom" })"#)
            .exec();
        assert!(result.is_err(), "registering 'paragraph' must be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("built-in") || err.contains("reserved"),
            "error should mention built-in collision, got: {err}"
        );

        // Also exercise a few more reserved names.
        for reserved in ["heading", "text", "list_item", "doc"] {
            let result = lua
                .load(format!(
                    r#"crap.richtext.register_node("{reserved}", {{ label = "x" }})"#
                ))
                .exec();
            assert!(
                result.is_err(),
                "registering reserved '{reserved}' must be rejected"
            );
        }
    }

    /// Regression: `crap.richtext.register_node` called outside the init
    /// phase must fail loudly. Each VM has its own
    /// `_crap_richtext_nodes` registry, so a runtime registration would
    /// only land in the current VM — admin renders served by other VMs
    /// in the pool would not see the node, producing intermittent
    /// rendering across requests.
    #[test]
    fn register_node_outside_init_phase_is_rejected() {
        let lua = Lua::new();
        let registry = Registry::shared();
        let crap = lua.create_table().unwrap();
        lua.globals().set("crap", crap.clone()).unwrap();
        register_fields(&lua).unwrap();
        register_richtext_init(&lua, Arc::clone(&registry)).unwrap();
        // No `set_app_data(InitPhase)` — simulating a runtime hook.

        let err = lua
            .load(
                r#"
            crap.richtext.register_node("widget", {
                label = "W",
                attrs = {},
            })
        "#,
            )
            .exec()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("init.lua") || err.contains("runtime"),
            "expected init-only error message, got: {err}"
        );

        let reg = registry.read().unwrap();
        assert!(
            reg.get_richtext_node("widget").is_none(),
            "node must NOT be registered when refused"
        );
    }
}
