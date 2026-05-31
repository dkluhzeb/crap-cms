//! Extract custom node instances (with their attribute values) from richtext
//! content. Supports both `ProseMirror` JSON and HTML serialisation formats.

use std::collections::HashMap;

use serde_json::Value;

use crate::core::{FieldDefinition, richtext::renderer::extract_attr_value};

/// A single extracted custom node instance with its attr values.
pub(super) struct NodeInstance {
    pub(super) node_type: String,
    pub(super) index: usize,
    pub(super) attrs: HashMap<String, Value>,
}

/// Extract custom node instances from `ProseMirror` JSON content.
pub(super) fn extract_nodes_from_json(
    json_str: &str,
    known_nodes: &HashMap<&str, &[FieldDefinition]>,
) -> Vec<NodeInstance> {
    let parsed: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut counters: HashMap<String, usize> = HashMap::new();
    let mut instances = Vec::new();
    collect_nodes_recursive(&parsed, known_nodes, &mut counters, &mut instances);
    instances
}

fn collect_nodes_recursive(
    value: &Value,
    known_nodes: &HashMap<&str, &[FieldDefinition]>,
    counters: &mut HashMap<String, usize>,
    out: &mut Vec<NodeInstance>,
) {
    let Some(obj) = value.as_object() else {
        return;
    };

    let Some(node_type) = obj.get("type").and_then(|t| t.as_str()) else {
        return;
    };

    if known_nodes.contains_key(node_type) {
        let idx = counters.entry(node_type.to_string()).or_insert(0);
        let current_idx = *idx;
        *idx += 1;

        let attrs = obj
            .get("attrs")
            .and_then(|a| a.as_object())
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();

        out.push(NodeInstance {
            node_type: node_type.to_string(),
            index: current_idx,
            attrs,
        });
    }

    if let Some(content) = obj.get("content").and_then(|c| c.as_array()) {
        for child in content {
            collect_nodes_recursive(child, known_nodes, counters, out);
        }
    }
}

/// Extract custom node instances from HTML content with `<crap-node>` tags.
pub(super) fn extract_nodes_from_html(
    html: &str,
    known_nodes: &HashMap<&str, &[FieldDefinition]>,
) -> Vec<NodeInstance> {
    let mut instances = Vec::new();
    let mut counters: HashMap<String, usize> = HashMap::new();
    let mut remaining = html;

    while let Some(start) = remaining.find("<crap-node ") {
        let after_start = &remaining[start..];
        let close_pos = match (after_start.find("/>"), after_start.find("</crap-node>")) {
            (Some(sc), Some(et)) => {
                if sc < et {
                    sc + "/>".len()
                } else {
                    et + "</crap-node>".len()
                }
            }
            (Some(sc), None) => sc + "/>".len(),
            (None, Some(et)) => et + "</crap-node>".len(),
            (None, None) => break,
        };

        let tag = &after_start[..close_pos];

        if let Some(node_type) = extract_attr_value(tag, "data-type")
            && known_nodes.contains_key(node_type.as_str())
        {
            let idx = counters.entry(node_type.clone()).or_insert(0);
            let current_idx = *idx;
            *idx += 1;

            let attrs: HashMap<String, Value> = extract_attr_value(tag, "data-attrs")
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();

            instances.push(NodeInstance {
                node_type,
                index: current_idx,
                attrs,
            });
        }

        remaining = &after_start[close_pos..];
    }

    instances
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn known() -> HashMap<&'static str, &'static [FieldDefinition]> {
        let mut m: HashMap<&str, &[FieldDefinition]> = HashMap::new();
        m.insert("callout", &[]);
        m
    }

    #[test]
    fn json_extracts_known_nodes_recursively_with_per_type_indices() {
        let content = json!({
            "type": "doc",
            "content": [
                { "type": "callout", "attrs": { "color": "red" } },
                { "type": "paragraph", "content": [
                    { "type": "callout", "attrs": { "color": "blue" } }
                ]},
                { "type": "unknown_node", "attrs": {} }
            ]
        })
        .to_string();

        let nodes = extract_nodes_from_json(&content, &known());
        assert_eq!(nodes.len(), 2, "two callouts, unknown node ignored");
        assert_eq!(nodes[0].node_type, "callout");
        assert_eq!(nodes[0].index, 0);
        assert_eq!(nodes[0].attrs.get("color"), Some(&json!("red")));
        // The nested callout keeps the per-type counter going.
        assert_eq!(nodes[1].index, 1);
        assert_eq!(nodes[1].attrs.get("color"), Some(&json!("blue")));
    }

    #[test]
    fn json_malformed_returns_empty() {
        assert!(extract_nodes_from_json("not json", &known()).is_empty());
    }

    #[test]
    fn html_extracts_known_nodes_with_attrs_and_indices() {
        let html = concat!(
            "<p>hi</p>",
            "<crap-node data-type=\"callout\" data-attrs='{\"color\":\"red\"}'></crap-node>",
            "<crap-node data-type=\"callout\" data-attrs='{\"color\":\"blue\"}' />",
            "<crap-node data-type=\"other\" data-attrs='{}'></crap-node>",
        );
        let nodes = extract_nodes_from_html(html, &known());
        assert_eq!(nodes.len(), 2, "two callouts, 'other' ignored");
        assert_eq!(nodes[0].index, 0);
        assert_eq!(nodes[0].attrs.get("color"), Some(&json!("red")));
        assert_eq!(nodes[1].index, 1);
        assert_eq!(nodes[1].attrs.get("color"), Some(&json!("blue")));
    }
}
