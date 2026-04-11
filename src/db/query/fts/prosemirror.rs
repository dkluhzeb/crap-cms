//! ProseMirror JSON text extraction helpers.

use std::collections::HashMap;

use serde_json::Value;

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
        let obj = match value.as_object() {
            Some(o) => o,
            None => return,
        };
        let node_type = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");

        if node_type == "text"
            && let Some(text) = obj.get("text").and_then(|t| t.as_str())
        {
            out.push(text.to_string());
        }

        // Check for custom node with searchable attrs
        if let Some(searchable) = node_searchable.get(node_type)
            && let Some(attrs) = obj.get("attrs").and_then(|a| a.as_object())
        {
            for attr_name in searchable {
                if let Some(val) = attrs.get(*attr_name).and_then(|v| v.as_str())
                    && !val.is_empty()
                {
                    out.push(val.to_string());
                }
            }
        }

        if let Some(content) = obj.get("content").and_then(|c| c.as_array()) {
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
