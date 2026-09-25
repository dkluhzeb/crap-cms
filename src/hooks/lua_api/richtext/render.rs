//! `crap.richtext.render` — rich text (either storage format, as read) to
//! HTML, with registered custom nodes rendered by their Lua `render`
//! functions.

use mlua::{Error::RuntimeError, FromLua, Function, Lua, Result as LuaResult, Table, Value};
use serde_json::Value as JsonValue;
use tracing::warn;

use crate::{
    core::richtext::{
        render_html_custom_nodes, render_prosemirror_document, render_prosemirror_to_html,
    },
    hooks::lua_api::{
        json_to_lua, lua_to_json,
        parse::{deny_unknown_keys, get_string_strict},
        utils::lua_err,
    },
    typegen::lua::LuaAnnotation,
};

/// Options for `crap.richtext.render(content, opts)`.
#[derive(LuaAnnotation, Default)]
#[lua(class = "crap.RichtextRenderOptions")]
pub(crate) struct RichtextRenderOptions {
    /// The content's storage format — the field's `admin.format`. Omit to
    /// detect it: a table, or a string holding a JSON document object
    /// (`"type": "doc"`), is JSON; any other string is HTML.
    #[lua(ty = "\"html\" | \"json\"", optional)]
    format: Option<RichtextFormat>,
}

/// A rich text storage format.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RichtextFormat {
    Html,
    Json,
}

impl FromLua for RichtextRenderOptions {
    fn from_lua(value: Value, _lua: &Lua) -> LuaResult<Self> {
        let tbl = match value {
            Value::Nil => return Ok(Self::default()),
            Value::Table(tbl) => tbl,
            other => {
                return Err(RuntimeError(format!(
                    "crap.richtext.render options must be a table, got {}",
                    other.type_name()
                )));
            }
        };

        deny_unknown_keys(&tbl, "crap.richtext.render options", &["format"]).map_err(lua_err)?;

        let format = match get_string_strict(&tbl, "format", "crap.richtext.render options")? {
            None => None,
            Some(f) if f == "html" => Some(RichtextFormat::Html),
            Some(f) if f == "json" => Some(RichtextFormat::Json),
            Some(f) => {
                return Err(RuntimeError(format!(
                    "crap.richtext.render options 'format': unknown value '{f}'. \
                     Valid values: html, json"
                )));
            }
        };

        Ok(Self { format })
    }
}

/// Render rich text to HTML: `nil` → `""`; a table → a JSON document; a
/// string → per `opts.format`, or detected (see [`RichtextRenderOptions`]).
pub(super) fn render(
    lua: &Lua,
    content: &Value,
    opts: &RichtextRenderOptions,
) -> LuaResult<String> {
    let storage: Table = lua.named_registry_value("_crap_richtext_nodes")?;
    let render_custom = custom_node_renderer(lua, &storage);

    match content {
        Value::Nil => Ok(String::new()),
        Value::String(s) => render_text(&s.to_str()?, opts.format, &render_custom),
        Value::Table(_) => {
            if opts.format == Some(RichtextFormat::Html) {
                return Err(RuntimeError(
                    "crap.richtext.render: a table is a JSON document, not HTML — \
                     pass HTML as a string"
                        .into(),
                ));
            }

            let doc = lua_to_json(content)?;
            require_document(&doc)?;

            Ok(render_prosemirror_document(&doc, &render_custom))
        }
        other => Err(RuntimeError(format!(
            "crap.richtext.render expects a string or a table, got {}",
            other.type_name()
        ))),
    }
}

/// Rich text held as a string: JSON document text or HTML.
fn render_text<F>(
    content: &str,
    format: Option<RichtextFormat>,
    render_custom: &F,
) -> LuaResult<String>
where
    F: Fn(&str, &JsonValue) -> Option<String>,
{
    let content = content.trim();

    if content.is_empty() {
        return Ok(String::new());
    }

    match format {
        Some(RichtextFormat::Json) => render_prosemirror_to_html(content, render_custom)
            .map_err(|e| RuntimeError(format!("Render error: {e:#}"))),
        Some(RichtextFormat::Html) => Ok(render_html_custom_nodes(content, render_custom)),
        None => Ok(match detect_document(content) {
            Some(doc) => render_prosemirror_document(&doc, render_custom),
            None => render_html_custom_nodes(content, render_custom),
        }),
    }
}

/// `content` as a JSON document when it is one — an object whose `type` is
/// `"doc"`. Anything else, including HTML or plain text that happens to begin
/// with `{`, is not.
fn detect_document(content: &str) -> Option<JsonValue> {
    if !content.starts_with('{') {
        return None;
    }

    serde_json::from_str::<JsonValue>(content)
        .ok()
        .filter(is_document)
}

fn is_document(value: &JsonValue) -> bool {
    value.get("type").and_then(JsonValue::as_str) == Some("doc")
}

fn require_document(doc: &JsonValue) -> LuaResult<()> {
    if is_document(doc) {
        return Ok(());
    }

    Err(RuntimeError(
        "crap.richtext.render: the table is not a rich text document \
         (expected { type = \"doc\", content = { ... } })"
            .into(),
    ))
}

/// Render one custom node with its registered Lua `render` function; `None`
/// (the node passes through as `<crap-node>`) when the node has no render
/// function or it fails.
fn custom_node_renderer<'a>(
    lua: &'a Lua,
    storage: &'a Table,
) -> impl Fn(&str, &JsonValue) -> Option<String> + 'a {
    move |node_type, attrs| {
        let entry: Table = storage.get(node_type).ok()?;
        let render_fn: Function = entry.get("render").ok()?;
        let attrs_lua = json_to_lua(lua, attrs).ok()?;

        render_fn
            .call::<String>(attrs_lua)
            .inspect_err(|e| warn!("Render function for '{node_type}' failed: {e}"))
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::setup_lua;

    fn eval(lua: &mlua::Lua, code: &str) -> String {
        lua.load(code).eval().unwrap()
    }

    fn register_cta(lua: &mlua::Lua) {
        lua.load(
            r#"
            crap.richtext.register_node("cta", {
                label = "CTA",
                attrs = {
                    crap.fields.text({ name = "text" }),
                    crap.fields.text({ name = "url" }),
                },
                render = function(attrs)
                    return '<a href="' .. attrs.url .. '">' .. attrs.text .. '</a>'
                end,
            })
        "#,
        )
        .exec()
        .unwrap();
    }

    #[test]
    fn render_json_with_custom_nodes() {
        let (lua, _) = setup_lua();
        register_cta(&lua);

        let result = eval(
            &lua,
            r#"return crap.richtext.render('{"type":"doc","content":[{"type":"cta","attrs":{"text":"Click","url":"/go"}}]}')"#,
        );
        assert_eq!(result, r#"<a href="/go">Click</a>"#);
    }

    /// Regression: a JSON-format field reads as a table, and `render` only
    /// took a string — the documented `crap.richtext.render(doc.body)` raised.
    #[test]
    fn render_accepts_the_document_table_a_json_field_reads_as() {
        let (lua, _) = setup_lua();
        register_cta(&lua);

        let result = eval(
            &lua,
            r#"return crap.richtext.render({ type = "doc", content = {
                { type = "paragraph", content = { { type = "text", text = "Hi" } } },
                { type = "cta", attrs = { text = "Go", url = "/x" } },
            } })"#,
        );
        assert_eq!(result, r#"<p>Hi</p><a href="/x">Go</a>"#);
    }

    #[test]
    fn render_refuses_a_table_that_is_not_a_document() {
        let (lua, _) = setup_lua();

        let err = lua
            .load(r#"return crap.richtext.render({ type = "paragraph" })"#)
            .eval::<String>()
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a rich text document"), "{err}");
    }

    #[test]
    fn render_html_with_custom_nodes() {
        let (lua, _) = setup_lua();
        register_cta(&lua);

        let result = eval(
            &lua,
            r#"return crap.richtext.render('<p>Hi</p><crap-node data-type="cta" data-attrs=\'{"text":"Go","url":"/g"}\'></crap-node>')"#,
        );
        assert_eq!(result, r#"<p>Hi</p><a href="/g">Go</a>"#);
    }

    /// Regression: any string starting with `{` was parsed as JSON, so HTML
    /// or plain text such as "{name} joined" raised a render error.
    #[test]
    fn a_string_that_is_not_a_json_document_renders_as_html() {
        let (lua, _) = setup_lua();

        assert_eq!(
            eval(&lua, r#"return crap.richtext.render("{name} joined")"#),
            "{name} joined"
        );
        assert_eq!(
            eval(&lua, r#"return crap.richtext.render('{"a":1}')"#),
            r#"{"a":1}"#
        );
    }

    #[test]
    fn an_explicit_format_is_honoured() {
        let (lua, _) = setup_lua();

        assert_eq!(
            eval(
                &lua,
                r#"return crap.richtext.render('{"type":"doc"}', { format = "html" })"#
            ),
            r#"{"type":"doc"}"#
        );

        let err = lua
            .load(r#"return crap.richtext.render("{not valid json", { format = "json" })"#)
            .eval::<String>()
            .unwrap_err()
            .to_string();
        assert!(err.contains("Render error"), "{err}");

        let err = lua
            .load(r#"return crap.richtext.render("<p>x</p>", { format = "markdown" })"#)
            .eval::<String>()
            .unwrap_err()
            .to_string();
        assert!(err.contains("markdown"), "{err}");

        let err = lua
            .load(r#"return crap.richtext.render("<p>x</p>", { formt = "html" })"#)
            .eval::<String>()
            .unwrap_err()
            .to_string();
        assert!(err.contains("formt"), "{err}");
    }

    #[test]
    fn empty_and_nil_render_empty() {
        let (lua, _) = setup_lua();

        assert_eq!(eval(&lua, r#"return crap.richtext.render("")"#), "");
        assert_eq!(eval(&lua, "return crap.richtext.render(nil)"), "");
    }

    /// A registered node with NO render function passes through as
    /// `<crap-node>`.
    #[test]
    fn render_json_node_without_render_function_passthrough() {
        let (lua, _) = setup_lua();
        lua.load(
            r#"crap.richtext.register_node("badge", {
                attrs = { crap.fields.text({ name = "text" }) },
            })"#,
        )
        .exec()
        .unwrap();

        let result = eval(
            &lua,
            r#"return crap.richtext.render('{"type":"doc","content":[{"type":"badge","attrs":{"text":"hi"}}]}')"#,
        );
        assert!(result.contains(r#"data-type="badge""#), "{result}");
    }

    /// A node type that was never registered passes through too.
    #[test]
    fn render_json_unregistered_node_passthrough() {
        let (lua, _) = setup_lua();

        let result = eval(
            &lua,
            r#"return crap.richtext.render('{"type":"doc","content":[{"type":"mystery","attrs":{"x":"y"}}]}')"#,
        );
        assert!(result.contains(r#"data-type="mystery""#), "{result}");
    }

    /// A render function that raises falls back to the passthrough.
    #[test]
    fn render_json_render_function_error_falls_back_to_passthrough() {
        let (lua, _) = setup_lua();
        lua.load(
            r#"crap.richtext.register_node("boom", {
                render = function() error("intentional render error") end,
            })"#,
        )
        .exec()
        .unwrap();

        let result = eval(
            &lua,
            r#"return crap.richtext.render('{"type":"doc","content":[{"type":"boom","attrs":{}}]}')"#,
        );
        assert!(result.contains(r#"data-type="boom""#), "{result}");
    }

    #[test]
    fn render_refuses_other_value_types() {
        let (lua, _) = setup_lua();

        let err = lua
            .load("return crap.richtext.render(42)")
            .eval::<String>()
            .unwrap_err()
            .to_string();
        assert!(err.contains("string or a table"), "{err}");
    }
}
