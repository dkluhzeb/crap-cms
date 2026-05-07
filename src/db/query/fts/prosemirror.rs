//! ProseMirror JSON text extraction helpers.

use std::collections::HashMap;

use serde_json::{Map, Value};

/// Borrow view over one ProseMirror node. The wire format is intentionally
/// open-ended (custom node types add arbitrary attrs), but the four fields
/// used by the FTS extractor — `type`, `text`, `attrs`, `content` — are
/// well-defined. This view returns `None` for any field the underlying
/// JSON object lacks, leaving the rest of the call site free of
/// `.get(...).and_then(...)` chains.
struct ProseMirrorNode<'a> {
    node_type: &'a str,
    text: Option<&'a str>,
    attrs: Option<&'a Map<String, Value>>,
    content: Option<&'a [Value]>,
}

impl<'a> ProseMirrorNode<'a> {
    /// Read a node out of a JSON value. Returns `None` when `value` is not
    /// an object.
    fn from_value(value: &'a Value) -> Option<Self> {
        let obj = value.as_object()?;
        Some(Self {
            node_type: obj.get("type").and_then(Value::as_str).unwrap_or(""),
            text: obj.get("text").and_then(Value::as_str),
            attrs: obj.get("attrs").and_then(Value::as_object),
            content: obj
                .get("content")
                .and_then(Value::as_array)
                .map(Vec::as_slice),
        })
    }
}

/// Extract plain text from a ProseMirror JSON document.
///
/// Recursively walks the JSON tree collecting `{ "type": "text", "text": "..." }` nodes.
/// Returns concatenated plain text with spaces between nodes.
/// Returns an empty string for invalid input.
///
/// Shorthand for [`extract_prosemirror_text_with_nodes`] with no custom nodes.
pub fn extract_prosemirror_text(json_str: &str) -> String {
    extract_prosemirror_text_with_nodes(json_str, &HashMap::new())
}

/// Extract text from ProseMirror JSON, including text from custom node attrs.
///
/// `node_searchable` maps node type names to their searchable attribute names.
/// When a node matches, its attr values are extracted as text in addition to
/// walking children.
pub fn extract_prosemirror_text_with_nodes(
    json_str: &str,
    node_searchable: &HashMap<&str, Vec<&str>>,
) -> String {
    fn collect_text_with_nodes(
        value: &Value,
        node_searchable: &HashMap<&str, Vec<&str>>,
        out: &mut Vec<String>,
    ) {
        let Some(node) = ProseMirrorNode::from_value(value) else {
            return;
        };

        if node.node_type == "text"
            && let Some(text) = node.text
        {
            out.push(text.to_string());
        }

        // Custom node with searchable attrs — pull each opted-in attr value.
        if let Some(searchable) = node_searchable.get(node.node_type)
            && let Some(attrs) = node.attrs
        {
            for attr_name in searchable {
                if let Some(val) = attrs.get(*attr_name).and_then(Value::as_str)
                    && !val.is_empty()
                {
                    out.push(val.to_string());
                }
            }
        }

        if let Some(content) = node.content {
            for child in content {
                collect_text_with_nodes(child, node_searchable, out);
            }
        }
    }

    let parsed: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let mut parts = Vec::new();

    collect_text_with_nodes(&parsed, node_searchable, &mut parts);

    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_prosemirror_text_simple() {
        let json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello world"}]}]}"#;
        assert_eq!(extract_prosemirror_text(json), "Hello world");
    }

    #[test]
    fn extract_prosemirror_text_nested() {
        let json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello"},{"type":"text","text":" world","marks":[{"type":"strong"}]}]},{"type":"paragraph","content":[{"type":"text","text":"Second paragraph"}]}]}"#;
        assert_eq!(
            extract_prosemirror_text(json),
            "Hello  world Second paragraph"
        );
    }

    #[test]
    fn extract_prosemirror_text_empty() {
        let json = r#"{"type":"doc","content":[]}"#;
        assert_eq!(extract_prosemirror_text(json), "");
    }

    #[test]
    fn extract_prosemirror_text_invalid() {
        assert_eq!(extract_prosemirror_text("not json"), "");
        assert_eq!(extract_prosemirror_text(""), "");
    }

    #[test]
    fn extract_prosemirror_text_with_custom_node_attrs() {
        let json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello"}]},{"type":"cta","attrs":{"text":"Click me","url":"/go"}}]}"#;
        let mut node_searchable = HashMap::new();
        node_searchable.insert("cta", vec!["text"]);
        let result = extract_prosemirror_text_with_nodes(json, &node_searchable);
        assert!(result.contains("Hello"));
        assert!(result.contains("Click me"));
        assert!(!result.contains("/go")); // url is not searchable
    }

    #[test]
    fn extract_prosemirror_text_ignores_non_searchable_attrs() {
        let json = r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"Button","url":"https://example.com","style":"primary"}}]}"#;
        let mut node_searchable = HashMap::new();
        node_searchable.insert("cta", vec!["text"]);
        let result = extract_prosemirror_text_with_nodes(json, &node_searchable);
        assert_eq!(result, "Button");
    }

    #[test]
    fn extract_prosemirror_text_with_nodes_empty_map() {
        // With empty map, behaves like the regular extract
        let json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello"}]}]}"#;
        let node_searchable = HashMap::new();
        let result = extract_prosemirror_text_with_nodes(json, &node_searchable);
        assert_eq!(result, "Hello");
    }

    #[test]
    fn extract_prosemirror_text_with_nodes_invalid_json() {
        let node_searchable = HashMap::new();
        let result = extract_prosemirror_text_with_nodes("not valid json", &node_searchable);
        assert_eq!(result, "", "invalid JSON should return empty string");
    }

    #[test]
    fn extract_prosemirror_text_with_nodes_empty_string() {
        let node_searchable = HashMap::new();
        let result = extract_prosemirror_text_with_nodes("", &node_searchable);
        assert_eq!(result, "");
    }
}
