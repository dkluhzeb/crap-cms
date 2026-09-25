//! The `crap.richtext` Lua table: `register_node` (init VM and pool VM
//! variants) and `render`, plus the entry points that install it.

use std::sync::Arc;

use mlua::{Lua, Result as LuaResult, Table, Value};

use super::{
    render::{RichtextRenderOptions, render},
    spec::{RichtextNodeSpec, validate_spec},
};
use crate::{
    core::{Registry, RichtextNodeDef, SharedRegistry},
    hooks::lua_api::utils::{registry_lock_poisoned, require_init_phase},
    typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

const REGISTER_NODE_INIT_ONLY_ERROR: &str = "crap.richtext.register_node must be called from \
     init.lua or a definition file — runtime registration only lands in one VM of the pool";

/// Stores a node entry (label, inline flag, optional render function) in the
/// VM's Lua registry, where `render` finds the node's render function.
fn store_node_in_lua(lua: &Lua, name: &str, spec: &RichtextNodeSpec) -> LuaResult<()> {
    let storage: Table = lua.named_registry_value("_crap_richtext_nodes")?;

    let node_entry = lua.create_table()?;
    node_entry.set("label", node_label(name, spec))?;
    node_entry.set("inline", spec.inline)?;

    if let Some(render_fn) = &spec.render {
        node_entry.set("render", render_fn.clone())?;
    }

    storage.set(name, node_entry)
}

fn node_label(name: &str, spec: &RichtextNodeSpec) -> String {
    spec.label.clone().unwrap_or_else(|| name.to_string())
}

/// Handles the `crap.richtext.register_node(name, spec)` call — validates
/// input, and stores the node definition in both Lua and Rust registries.
/// Attr parsing already happened in `RichtextNodeSpec::from_lua`.
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
    validate_spec(name, &spec)?;

    store_node_in_lua(lua, name, &spec)?;

    let def = RichtextNodeDef::builder(name, node_label(name, &spec))
        .inline(spec.inline)
        .has_render(spec.render.is_some())
        .attrs(spec.attrs)
        .searchable_attrs(spec.searchable_attrs)
        .build();

    let mut reg = registry.write().map_err(registry_lock_poisoned)?;
    reg.register_richtext_node(def);

    Ok(())
}

/// Pool-VM `register_node`: the same checks as the init VM (so a pool VM
/// refuses exactly what the init VM refuses), then the per-VM Lua-side
/// storage `render` needs. The shared registry was already populated by the
/// `init_lua` VM.
fn register_node_pool(lua: &Lua, name: &str, spec: &RichtextNodeSpec) -> LuaResult<()> {
    require_init_phase(lua, REGISTER_NODE_INIT_ONLY_ERROR)?;
    validate_spec(name, spec)?;

    store_node_in_lua(lua, name, spec)
}

// ── User-facing fns ──────────────────────────────────────────────────

/// Register a custom `ProseMirror` node type.
#[lua_fn(path = "crap.richtext.register_node")]
fn richtext_register_node_init(
    state: &SharedRegistry,
    lua: &Lua,
    #[lua(doc = "Node name (lowercase letters, digits and underscores).")] name: String,
    #[lua(ty = "crap.RichtextNodeSpec", doc = "Node specification.")] spec: RichtextNodeSpec,
) -> LuaResult<()> {
    register_node(lua, state, &name, spec)
}

/// Pool-VM variant of `register_node`. Same `InitPhase` guard and checks,
/// does the per-VM Lua-side storage so `render` can find the node's render
/// function, but skips the shared-registry write — the `init_lua` VM
/// already populated the registry.
#[lua_fn(path = "crap.richtext.register_node")]
fn richtext_register_node_pool(
    _state: &(),
    lua: &Lua,
    name: String,
    #[lua(ty = "crap.RichtextNodeSpec")] spec: RichtextNodeSpec,
) -> LuaResult<()> {
    register_node_pool(lua, &name, &spec)
}

/// Render rich text to HTML, replacing custom nodes with their rendered HTML.
/// Takes a field's value as read: a JSON-format field's document table, or
/// the string of either format. A string's format is `opts.format` when
/// given, otherwise detected — JSON only when it holds a document object
/// (`"type": "doc"`), HTML otherwise. `nil` renders as `""`.
#[lua_fn(path = "crap.richtext.render", returns_doc = "Rendered HTML output.")]
fn richtext_render(
    lua: &Lua,
    #[lua(
        ty = "string|table|nil",
        doc = "Rich text: HTML, `ProseMirror` JSON text, or a JSON document table."
    )]
    content: Value,
    #[lua(ty = "crap.RichtextRenderOptions", doc = "Rendering options.")] opts: Option<
        RichtextRenderOptions,
    >,
) -> LuaResult<String> {
    render(lua, &content, &opts.unwrap_or_default())
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
pub fn register_richtext_pool_init(lua: &Lua, _registry: Arc<Registry>) -> anyhow::Result<()> {
    let nodes_storage = lua.create_table()?;
    lua.set_named_registry_value("_crap_richtext_nodes", nodes_storage)?;
    register_crap_richtext_pool(lua, ())?;
    register_crap_richtext_render(lua, ())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::setup_lua;
    use super::*;
    use crate::hooks::{lifecycle::InitPhase, lua_api::fields::register_fields};

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

    /// Regression: settings with no effect on a node attr were only logged,
    /// so the node loaded without the uniqueness, per-locale value or access
    /// rule its author asked for. Registration now fails naming them.
    #[test]
    fn register_node_rejects_inert_attr_settings() {
        let (lua, registry) = setup_lua();

        let err = lua
            .load(
                r#"
            crap.richtext.register_node("warn_test", {
                attrs = {
                    crap.fields.text({ name = "title", unique = true, index = true, localized = true }),
                },
            })
        "#,
            )
            .exec()
            .unwrap_err()
            .to_string();

        assert!(err.contains("unique, index, localized"), "{err}");
        assert!(
            registry
                .read()
                .unwrap()
                .get_richtext_node("warn_test")
                .is_none()
        );
    }

    /// Regression: the pool VM's `register_node` skipped the
    /// `searchable_attrs` check the init VM ran, so the two could disagree.
    #[test]
    fn pool_register_node_runs_the_same_checks() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        lua.globals().set("crap", crap).unwrap();
        register_fields(&lua).unwrap();
        register_richtext_pool_init(&lua, Arc::new(Registry::new())).unwrap();
        lua.set_app_data(InitPhase);

        let err = lua
            .load(
                r#"crap.richtext.register_node("article", {
                    attrs = { crap.fields.text({ name = "title" }) },
                    searchable_attrs = { "nonexistent" },
                })"#,
            )
            .exec()
            .unwrap_err()
            .to_string();

        assert!(err.contains("nonexistent"), "{err}");
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
