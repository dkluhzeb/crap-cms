//! Extract custom node instances (with their attribute values) from richtext
//! content. Supports both `ProseMirror` JSON and HTML serialisation formats.

use std::{borrow::Cow, collections::HashMap};

use serde_json::Value;

use crate::core::{FieldDefinition, richtext::find_crap_nodes};

/// Most nested containers (objects and arrays) a rich text document may have —
/// exactly what `serde_json`'s parser accepts in text (its recursion budget of
/// 128 is exhausted on entering the 128th container), applied to documents
/// sent as objects too so both forms agree.
const MAX_DOCUMENT_DEPTH: usize = 127;

/// A single extracted custom node instance with its attr values.
pub(super) struct NodeInstance {
    pub(super) node_type: String,
    pub(super) index: usize,
    pub(super) attrs: HashMap<String, Value>,
}

/// A JSON-format rich text value as the `ProseMirror` document it carries — the
/// one normalization every node-attr pass reads through. The value may arrive
/// as JSON text (the admin form) or as the document object itself (MCP, Lua
/// tables).
///
/// `None` when the value is not a document: text that does not parse (or nests
/// deeper than [`MAX_DOCUMENT_DEPTH`]), or any other JSON type. Such content
/// cannot be checked for its nodes, so validation refuses it.
pub(super) fn json_document(value: &Value) -> Option<Cow<'_, Value>> {
    let doc = match value {
        Value::String(s) => Cow::Owned(serde_json::from_str::<Value>(s).ok()?),
        Value::Object(_) => Cow::Borrowed(value),
        _ => return None,
    };

    (doc.is_object() && !exceeds_depth(&doc, MAX_DOCUMENT_DEPTH)).then_some(doc)
}

/// Whether `value` nests containers deeper than `remaining` levels.
fn exceeds_depth(value: &Value, remaining: usize) -> bool {
    match value {
        Value::Object(map) => {
            remaining == 0
                || map
                    .values()
                    .any(|child| exceeds_depth(child, remaining - 1))
        }
        Value::Array(items) => {
            remaining == 0
                || items
                    .iter()
                    .any(|child| exceeds_depth(child, remaining - 1))
        }
        _ => false,
    }
}

/// Extract custom node instances from a `ProseMirror` JSON document.
pub(super) fn extract_nodes_from_json(
    doc: &Value,
    known_nodes: &HashMap<&str, &[FieldDefinition]>,
) -> Vec<NodeInstance> {
    let mut counters: HashMap<String, usize> = HashMap::new();
    let mut instances = Vec::new();
    collect_nodes_recursive(doc, known_nodes, &mut counters, &mut instances);
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

/// Extract custom node instances from HTML content with `<crap-node>` tags,
/// located by the tokenizer the renderer uses.
pub(super) fn extract_nodes_from_html(
    html: &str,
    known_nodes: &HashMap<&str, &[FieldDefinition]>,
) -> Vec<NodeInstance> {
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

    /// Unparseable, non-document and over-deep values are not documents, so
    /// validation refuses them instead of finding no nodes in them.
    #[test]
    fn json_document_refuses_what_it_cannot_read() {
        assert!(json_document(&json!("not json")).is_none());
        assert!(json_document(&json!(42)).is_none());
        assert!(json_document(&json!("[1,2]")).is_none());

        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        let deep_doc = format!(r#"{{"type":"doc","content":{deep}}}"#);
        assert!(json_document(&json!(deep_doc)).is_none());
    }

    #[test]
    fn json_document_accepts_text_and_objects() {
        let doc = json!({ "type": "doc", "content": [] });

        assert_eq!(json_document(&doc).as_deref(), Some(&doc));
        assert_eq!(
            json_document(&json!(doc.to_string())).as_deref(),
            Some(&doc)
        );
    }

    /// A document of `levels` nested objects, the root included.
    fn nested_document(levels: usize) -> Value {
        let mut doc = json!({ "type": "doc" });
        for _ in 1..levels {
            doc = json!({ "type": "doc", "child": doc });
        }

        doc
    }

    /// The deepest document the parser reads as text is accepted as an object
    /// too; one level more is refused in both forms.
    #[test]
    fn text_and_object_documents_share_the_depth_limit() {
        let deepest = nested_document(127);
        let too_deep = nested_document(128);

        assert!(serde_json::from_str::<Value>(&deepest.to_string()).is_ok());
        assert!(serde_json::from_str::<Value>(&too_deep.to_string()).is_err());

        assert!(json_document(&deepest).is_some());
        assert!(json_document(&json!(deepest.to_string())).is_some());
        assert!(json_document(&too_deep).is_none());
        assert!(json_document(&json!(too_deep.to_string())).is_none());
    }

    #[test]
    fn deeply_nested_objects_are_refused() {
        let mut doc = json!({ "type": "text" });
        for _ in 0..200 {
            doc = json!({ "type": "paragraph", "content": [doc] });
        }

        assert!(json_document(&doc).is_none());
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
