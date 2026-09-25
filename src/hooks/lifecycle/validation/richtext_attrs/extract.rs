//! Extract custom node instances (with their attribute values) from richtext
//! content. Supports both `ProseMirror` JSON and HTML serialisation formats.

use std::collections::HashMap;

use serde_json::Value;

use crate::core::{
    FieldDefinition,
    richtext::{find_crap_nodes, parse_document},
};

/// A field's custom nodes that declare attrs: each node's attr definitions by
/// node name.
pub(super) type KnownNodes<'a> = HashMap<&'a str, &'a [FieldDefinition]>;

/// The known custom node instances in a rich text value of `field`, in its
/// format: a `ProseMirror` document (its text or the object itself) for a JSON
/// field, HTML text otherwise. A value of neither shape holds no nodes.
pub(super) fn extract_nodes(
    field: &FieldDefinition,
    content: &Value,
    known_nodes: &KnownNodes<'_>,
) -> Vec<NodeInstance> {
    if field.parses_json() {
        return parse_document(content)
            .map(|doc| extract_nodes_from_json(&doc, known_nodes))
            .unwrap_or_default();
    }

    content
        .as_str()
        .map(|html| extract_nodes_from_html(html, known_nodes))
        .unwrap_or_default()
}

/// A single extracted custom node instance with its attr values.
pub(super) struct NodeInstance {
    pub(super) node_type: String,
    pub(super) index: usize,
    pub(super) attrs: HashMap<String, Value>,
}

/// Extract custom node instances from a `ProseMirror` JSON document.
fn extract_nodes_from_json(doc: &Value, known_nodes: &KnownNodes<'_>) -> Vec<NodeInstance> {
    let mut counters: HashMap<String, usize> = HashMap::new();
    let mut instances = Vec::new();
    collect_nodes_recursive(doc, known_nodes, &mut counters, &mut instances);
    instances
}

fn collect_nodes_recursive(
    value: &Value,
    known_nodes: &KnownNodes<'_>,
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

/// Extract custom node instances from HTML content with `<crap-node>` tags,
/// located by the tokenizer the renderer uses.
fn extract_nodes_from_html(html: &str, known_nodes: &KnownNodes<'_>) -> Vec<NodeInstance> {
    let mut counters: HashMap<String, usize> = HashMap::new();

    find_crap_nodes(html)
        .into_iter()
        .filter_map(|tag| {
            let node_type = tag.node_type()?.to_string();
            if !known_nodes.contains_key(node_type.as_str()) {
                return None;
            }

            let idx = counters.entry(node_type.clone()).or_insert(0);
            let index = *idx;
            *idx += 1;

            Some(NodeInstance {
                node_type,
                index,
                attrs: tag.node_attrs().into_iter().collect(),
            })
        })
        .collect()
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

    fn known_cta() -> HashMap<&'static str, &'static [FieldDefinition]> {
        let mut m: HashMap<&str, &[FieldDefinition]> = HashMap::new();
        m.insert("cta", &[]);
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
        });

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

    #[test]
    fn extract_nodes_json_basic() {
        let json =
            r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"Click","url":"/go"}}]}"#;
        let known = known_cta();
        let instances = extract_nodes_from_json(&serde_json::from_str(json).unwrap(), &known);
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].node_type, "cta");
        assert_eq!(instances[0].index, 0);
        assert_eq!(instances[0].attrs.get("text").unwrap(), "Click");
    }

    #[test]
    fn extract_nodes_json_multiple() {
        let json = r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"A","url":"/a"}},{"type":"paragraph","content":[{"type":"text","text":"hi"}]},{"type":"cta","attrs":{"text":"B","url":"/b"}}]}"#;
        let known = known_cta();
        let instances = extract_nodes_from_json(&serde_json::from_str(json).unwrap(), &known);
        assert_eq!(instances.len(), 2);
        assert_eq!(instances[0].index, 0);
        assert_eq!(instances[1].index, 1);
    }

    #[test]
    fn extract_nodes_html_basic() {
        let html = r#"<p>Hi</p><crap-node data-type="cta" data-attrs='{"text":"Go","url":"/x"}'></crap-node>"#;
        let known = known_cta();
        let instances = extract_nodes_from_html(html, &known);
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].node_type, "cta");
        assert_eq!(instances[0].attrs.get("text").unwrap(), "Go");
    }

    #[test]
    fn extract_nodes_html_multiple_with_correct_indexing() {
        let html = concat!(
            r#"<p>Start</p>"#,
            r#"<crap-node data-type="cta" data-attrs='{"text":"A","url":"/a"}'></crap-node>"#,
            r#"<p>Middle</p>"#,
            r#"<crap-node data-type="cta" data-attrs='{"text":"B","url":"/b"}'></crap-node>"#,
            r#"<p>End</p>"#,
        );
        let known = known_cta();

        let instances = extract_nodes_from_html(html, &known);
        assert_eq!(instances.len(), 2);
        assert_eq!(instances[0].index, 0);
        assert_eq!(instances[0].attrs.get("text").unwrap(), "A");
        assert_eq!(instances[1].index, 1);
        assert_eq!(instances[1].attrs.get("text").unwrap(), "B");
    }

    #[test]
    fn extract_nodes_json_nested_deep_tree() {
        // CTA inside a blockquote inside a list item
        let json = r#"{
            "type": "doc",
            "content": [
                {
                    "type": "bullet_list",
                    "content": [
                        {
                            "type": "list_item",
                            "content": [
                                {
                                    "type": "blockquote",
                                    "content": [
                                        {"type": "cta", "attrs": {"text": "Deep", "url": "/deep"}}
                                    ]
                                }
                            ]
                        }
                    ]
                }
            ]
        }"#;
        let known = known_cta();

        let instances = extract_nodes_from_json(&serde_json::from_str(json).unwrap(), &known);
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].attrs.get("text").unwrap(), "Deep");
    }

    #[test]
    fn extract_nodes_html_mixed_self_closing_and_full() {
        let html = concat!(
            r#"<p>A</p>"#,
            r#"<crap-node data-type="cta" data-attrs='{"text":"SC","url":"/sc"}'/>"#,
            r#"<p>B</p>"#,
            r#"<crap-node data-type="cta" data-attrs='{"text":"Full","url":"/full"}'></crap-node>"#,
        );
        let known = known_cta();

        let instances = extract_nodes_from_html(html, &known);
        assert_eq!(
            instances.len(),
            2,
            "both self-closing and full tags extracted"
        );
        assert_eq!(instances[0].attrs.get("text").unwrap(), "SC");
        assert_eq!(instances[1].attrs.get("text").unwrap(), "Full");
    }

    #[test]
    fn extract_nodes_html_self_closing_tag() {
        let html =
            r#"<p>Test</p><crap-node data-type="cta" data-attrs='{"text":"Go","url":"/x"}'/>"#;
        let known = known_cta();

        let instances = extract_nodes_from_html(html, &known);
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].attrs.get("text").unwrap(), "Go");
    }

    #[test]
    fn extract_nodes_html_unknown_node_skipped() {
        let html = r#"<crap-node data-type="unknown" data-attrs='{"x":"y"}'></crap-node>"#;
        let known = known_cta();

        let instances = extract_nodes_from_html(html, &known);
        assert!(instances.is_empty(), "unknown node types should be skipped");
    }
}
