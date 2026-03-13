//! Custom ProseMirror node types for richtext fields.
//!
//! Provides data model types for defining custom structured nodes (CTAs, embeds,
//! alerts, etc.) that can be embedded inside richtext content. Also includes a
//! ProseMirror JSON → HTML renderer that handles both standard PM nodes and
//! custom nodes via a callback.

pub mod node_attr;
pub mod node_attr_builder;
pub mod richtext_node_def;
pub mod richtext_node_def_builder;

pub use node_attr::{NodeAttr, NodeAttrType};
pub use node_attr_builder::NodeAttrBuilder;
pub use richtext_node_def::RichtextNodeDef;
pub use richtext_node_def_builder::RichtextNodeDefBuilder;

use serde_json::{Map, Value};

/// Render ProseMirror JSON to HTML.
///
/// Handles standard PM nodes (doc, paragraph, heading, text, blockquote, code_block,
/// bullet_list, ordered_list, list_item, horizontal_rule, hard_break) and marks
/// (strong, em, code, link). Custom nodes are delegated to the `custom_renderer`
/// callback which receives (node_type, attrs_json) and returns HTML.
///
/// For unknown nodes without a custom renderer, emits `<crap-node>` passthrough.
pub fn render_prosemirror_to_html<F>(json_str: &str, custom_renderer: &F) -> Result<String, String>
where
    F: Fn(&str, &Value) -> Option<String>,
{
    let parsed: Value =
        serde_json::from_str(json_str).map_err(|e| format!("Invalid JSON: {}", e))?;
    let mut out = String::new();
    render_node(&parsed, custom_renderer, &mut out);
    Ok(out)
}

fn render_node<F>(node: &Value, custom_renderer: &F, out: &mut String)
where
    F: Fn(&str, &Value) -> Option<String>,
{
    let obj = match node.as_object() {
        Some(o) => o,
        None => return,
    };

    let node_type = match obj.get("type").and_then(|t| t.as_str()) {
        Some(t) => t,
        None => return,
    };

    match node_type {
        "doc" => render_children(node, custom_renderer, out),
        "paragraph" => {
            out.push_str("<p>");
            render_children(node, custom_renderer, out);
            out.push_str("</p>");
        }
        "heading" => {
            let level = obj
                .get("attrs")
                .and_then(|a| a.get("level"))
                .and_then(|l| l.as_u64())
                .unwrap_or(1)
                .min(6);
            out.push_str(&format!("<h{}>", level));
            render_children(node, custom_renderer, out);
            out.push_str(&format!("</h{}>", level));
        }
        "text" => {
            let text = obj.get("text").and_then(|t| t.as_str()).unwrap_or("");
            let marks = obj.get("marks").and_then(|m| m.as_array());
            render_text_with_marks(text, marks, out);
        }
        "blockquote" => {
            out.push_str("<blockquote>");
            render_children(node, custom_renderer, out);
            out.push_str("</blockquote>");
        }
        "code_block" => {
            out.push_str("<pre><code>");
            render_children(node, custom_renderer, out);
            out.push_str("</code></pre>");
        }
        "bullet_list" => {
            out.push_str("<ul>");
            render_children(node, custom_renderer, out);
            out.push_str("</ul>");
        }
        "ordered_list" => {
            out.push_str("<ol>");
            render_children(node, custom_renderer, out);
            out.push_str("</ol>");
        }
        "list_item" => {
            out.push_str("<li>");
            render_children(node, custom_renderer, out);
            out.push_str("</li>");
        }
        "horizontal_rule" => out.push_str("<hr>"),
        "hard_break" => out.push_str("<br>"),
        _ => {
            // Custom node — try the callback
            let attrs = obj
                .get("attrs")
                .cloned()
                .unwrap_or(Value::Object(Map::new()));

            if let Some(html) = custom_renderer(node_type, &attrs) {
                out.push_str(&html);
            } else {
                // Passthrough as <crap-node>
                let attrs_json = serde_json::to_string(&attrs).unwrap_or_default();
                out.push_str(&format!(
                    "<crap-node data-type=\"{}\" data-attrs='{}'>",
                    html_escape(node_type),
                    html_escape_attr(&attrs_json)
                ));
                out.push_str("</crap-node>");
            }
        }
    }
}

fn render_children<F>(node: &Value, custom_renderer: &F, out: &mut String)
where
    F: Fn(&str, &Value) -> Option<String>,
{
    if let Some(content) = node.get("content").and_then(|c| c.as_array()) {
        for child in content {
            render_node(child, custom_renderer, out);
        }
    }
}

fn render_text_with_marks(text: &str, marks: Option<&Vec<Value>>, out: &mut String) {
    let escaped = html_escape(text);
    match marks {
        None => {
            out.push_str(&escaped);
        }
        Some(marks) if marks.is_empty() => {
            out.push_str(&escaped);
        }
        Some(marks) => {
            let mut open_tags = Vec::new();
            for mark in marks {
                let mark_type = mark.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match mark_type {
                    "strong" => {
                        out.push_str("<strong>");
                        open_tags.push("</strong>");
                    }
                    "em" => {
                        out.push_str("<em>");
                        open_tags.push("</em>");
                    }
                    "code" => {
                        out.push_str("<code>");
                        open_tags.push("</code>");
                    }
                    "link" => {
                        let href = mark
                            .get("attrs")
                            .and_then(|a| a.get("href"))
                            .and_then(|h| h.as_str())
                            .unwrap_or("#");
                        out.push_str(&format!("<a href=\"{}\">", html_escape_attr(href)));
                        open_tags.push("</a>");
                    }
                    _ => {} // Unknown mark — skip
                }
            }
            out.push_str(&escaped);
            for tag in open_tags.into_iter().rev() {
                out.push_str(tag);
            }
        }
    }
}

/// Render HTML content with custom node replacement.
///
/// Finds `<crap-node data-type="X" data-attrs='Y'></crap-node>` elements and
/// replaces them with the output of `custom_renderer(name, attrs)`. Elements
/// without a matching renderer are left unchanged.
pub fn render_html_custom_nodes<F>(html: &str, custom_renderer: &F) -> String
where
    F: Fn(&str, &Value) -> Option<String>,
{
    let mut result = String::with_capacity(html.len());
    let mut remaining = html;

    while let Some(start) = remaining.find("<crap-node ") {
        // Add everything before the tag
        result.push_str(&remaining[..start]);

        // Find the end of the opening tag
        let after_start = &remaining[start..];
        let close_pos = if let Some(p) = after_start.find("</crap-node>") {
            p + "</crap-node>".len()
        } else if let Some(p) = after_start.find("/>") {
            p + "/>".len()
        } else {
            // Malformed — just pass through the rest
            result.push_str(after_start);
            remaining = "";
            continue;
        };

        let tag = &after_start[..close_pos];

        // Extract data-type
        let node_type = extract_attr_value(tag, "data-type");
        // Extract data-attrs
        let attrs_str = extract_attr_value(tag, "data-attrs");

        if let Some(ref nt) = node_type {
            let attrs: Value = attrs_str
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Object(Map::new()));

            if let Some(rendered) = custom_renderer(nt, &attrs) {
                result.push_str(&rendered);
            } else {
                // No renderer — pass through
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

/// Extract an attribute value from a tag string. Handles both single and double quotes.
fn extract_attr_value(tag: &str, attr_name: &str) -> Option<String> {
    let patterns = [format!("{}=\"", attr_name), format!("{}='", attr_name)];

    for pattern in &patterns {
        if let Some(start) = tag.find(pattern.as_str()) {
            let value_start = start + pattern.len();
            let quote_char = if pattern.ends_with('"') { '"' } else { '\'' };

            if let Some(end) = tag[value_start..].find(quote_char) {
                return Some(tag[value_start..value_start + end].to_string());
            }
        }
    }

    None
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn html_escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_custom(_name: &str, _attrs: &Value) -> Option<String> {
        None
    }

    #[test]
    fn render_empty_doc() {
        let json = r#"{"type":"doc","content":[]}"#;
        let result = render_prosemirror_to_html(json, &no_custom).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn render_paragraph() {
        let json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello world"}]}]}"#;
        let result = render_prosemirror_to_html(json, &no_custom).unwrap();
        assert_eq!(result, "<p>Hello world</p>");
    }

    #[test]
    fn render_heading() {
        let json = r#"{"type":"doc","content":[{"type":"heading","attrs":{"level":2},"content":[{"type":"text","text":"Title"}]}]}"#;
        let result = render_prosemirror_to_html(json, &no_custom).unwrap();
        assert_eq!(result, "<h2>Title</h2>");
    }

    #[test]
    fn render_marks() {
        let json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"bold","marks":[{"type":"strong"}]},{"type":"text","text":" and "},{"type":"text","text":"italic","marks":[{"type":"em"}]}]}]}"#;
        let result = render_prosemirror_to_html(json, &no_custom).unwrap();
        assert_eq!(result, "<p><strong>bold</strong> and <em>italic</em></p>");
    }

    #[test]
    fn render_link_mark() {
        let json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"click","marks":[{"type":"link","attrs":{"href":"https://example.com"}}]}]}]}"#;
        let result = render_prosemirror_to_html(json, &no_custom).unwrap();
        assert_eq!(result, r#"<p><a href="https://example.com">click</a></p>"#);
    }

    #[test]
    fn render_custom_node_with_callback() {
        let json =
            r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"Click me","url":"/go"}}]}"#;
        let renderer = |name: &str, attrs: &Value| -> Option<String> {
            if name == "cta" {
                let text = attrs.get("text").and_then(|t| t.as_str()).unwrap_or("");
                let url = attrs.get("url").and_then(|u| u.as_str()).unwrap_or("#");
                Some(format!("<a href=\"{}\" class=\"btn\">{}</a>", url, text))
            } else {
                None
            }
        };
        let result = render_prosemirror_to_html(json, &renderer).unwrap();
        assert_eq!(result, r#"<a href="/go" class="btn">Click me</a>"#);
    }

    #[test]
    fn render_custom_node_passthrough() {
        let json = r#"{"type":"doc","content":[{"type":"unknown_widget","attrs":{"foo":"bar"}}]}"#;
        let result = render_prosemirror_to_html(json, &no_custom).unwrap();
        assert!(result.contains("crap-node"));
        assert!(result.contains("data-type=\"unknown_widget\""));
        assert!(result.contains("data-attrs="));
    }

    #[test]
    fn render_nested() {
        let json = r#"{"type":"doc","content":[{"type":"blockquote","content":[{"type":"paragraph","content":[{"type":"text","text":"quoted"}]}]},{"type":"bullet_list","content":[{"type":"list_item","content":[{"type":"paragraph","content":[{"type":"text","text":"item 1"}]}]},{"type":"list_item","content":[{"type":"paragraph","content":[{"type":"text","text":"item 2"}]}]}]}]}"#;
        let result = render_prosemirror_to_html(json, &no_custom).unwrap();
        assert_eq!(
            result,
            "<blockquote><p>quoted</p></blockquote><ul><li><p>item 1</p></li><li><p>item 2</p></li></ul>"
        );
    }

    #[test]
    fn render_code_block() {
        let json = r#"{"type":"doc","content":[{"type":"code_block","content":[{"type":"text","text":"let x = 1;"}]}]}"#;
        let result = render_prosemirror_to_html(json, &no_custom).unwrap();
        assert_eq!(result, "<pre><code>let x = 1;</code></pre>");
    }

    #[test]
    fn render_horizontal_rule_and_hard_break() {
        let json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"before"},{"type":"hard_break"},{"type":"text","text":"after"}]},{"type":"horizontal_rule"}]}"#;
        let result = render_prosemirror_to_html(json, &no_custom).unwrap();
        assert_eq!(result, "<p>before<br>after</p><hr>");
    }

    #[test]
    fn render_html_escape() {
        let json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"<script>alert('xss')</script>"}]}]}"#;
        let result = render_prosemirror_to_html(json, &no_custom).unwrap();
        assert!(!result.contains("<script>"));
        assert!(result.contains("&lt;script&gt;"));
    }

    #[test]
    fn render_invalid_json() {
        let result = render_prosemirror_to_html("not json", &no_custom);
        assert!(result.is_err());
    }

    #[test]
    fn html_custom_nodes_replacement() {
        let html = r#"<p>Before</p><crap-node data-type="cta" data-attrs='{"text":"Go","url":"/x"}'></crap-node><p>After</p>"#;
        let renderer = |name: &str, attrs: &Value| -> Option<String> {
            if name == "cta" {
                let text = attrs.get("text").and_then(|t| t.as_str()).unwrap_or("");
                Some(format!("<button>{}</button>", text))
            } else {
                None
            }
        };
        let result = render_html_custom_nodes(html, &renderer);
        assert_eq!(result, "<p>Before</p><button>Go</button><p>After</p>");
    }

    #[test]
    fn html_custom_nodes_passthrough() {
        let html = r#"<crap-node data-type="unknown" data-attrs='{}'></crap-node>"#;
        let result = render_html_custom_nodes(html, &no_custom);
        assert_eq!(result, html);
    }

    #[test]
    fn html_no_crap_nodes() {
        let html = "<p>Just plain HTML</p>";
        let result = render_html_custom_nodes(html, &no_custom);
        assert_eq!(result, html);
    }

    #[test]
    fn extract_attr_value_double_quotes() {
        let tag = r#"<crap-node data-type="cta" data-attrs='{"x":1}'></crap-node>"#;
        assert_eq!(
            extract_attr_value(tag, "data-type"),
            Some("cta".to_string())
        );
    }

    #[test]
    fn extract_attr_value_single_quotes() {
        let tag = "<crap-node data-type='hello'></crap-node>";
        assert_eq!(
            extract_attr_value(tag, "data-type"),
            Some("hello".to_string())
        );
    }

    #[test]
    fn extract_attr_value_missing() {
        let tag = "<crap-node></crap-node>";
        assert_eq!(extract_attr_value(tag, "data-type"), None);
    }

    #[test]
    fn node_attr_type_roundtrip() {
        for t in &[
            NodeAttrType::Text,
            NodeAttrType::Number,
            NodeAttrType::Select,
            NodeAttrType::Checkbox,
            NodeAttrType::Textarea,
        ] {
            assert_eq!(NodeAttrType::from_name(t.as_str()), *t);
        }
    }
}
