//! The `ProseMirror` document a JSON-format rich text field accepts.
//!
//! The admin editor builds its schema from the field's `admin.features` and
//! `admin.nodes` and loads a stored document with `Node.fromJSON`, which throws
//! on any node or mark type the schema lacks, and on an attribute value its
//! type refuses. A value the editor cannot load is refused at write time by
//! [`RichtextSchema::check`], which mirrors that schema's node and mark types
//! exactly — so every write surface (admin, gRPC, MCP, Lua) stores only
//! documents the editor can open.
//!
//! One deliberate step beyond the editor: an attribute its node or mark does
//! not declare is refused, where the editor silently drops it on load. A stale
//! attribute never blocks a save: an untouched admin field submits the value
//! as stored, which validation accepts as held, and an edit serializes the
//! document as loaded, without it.

use std::{borrow::Cow, collections::HashMap, fmt};

use serde_json::{Map, Value};

use crate::core::{FieldDefinition, Registry};

/// Every name `admin.features` accepts. An empty list enables all of them.
pub const RICHTEXT_FEATURES: &[&str] = &[
    "bold",
    "italic",
    "code",
    "link",
    "heading",
    "blockquote",
    "orderedList",
    "bulletList",
    "codeBlock",
    "horizontalRule",
];

/// Most nested containers (objects and arrays) a rich text document may have —
/// exactly what `serde_json`'s parser accepts in text (its recursion budget of
/// 128 is exhausted on entering the 128th container), applied to documents
/// sent as objects too so both forms agree.
const MAX_DOCUMENT_DEPTH: usize = 127;

/// A JSON-format rich text value as the document object it carries — the one
/// normalization every rich text pass reads through. The value may arrive as
/// JSON text (the admin form) or as the document object itself (MCP, Lua
/// tables).
///
/// `None` when the value is not a JSON object: text that does not parse (or
/// nests deeper than the parser allows), or any other JSON type.
#[must_use]
pub fn parse_document(value: &Value) -> Option<Cow<'_, Value>> {
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
            remaining == 0 || map.values().any(|c| exceeds_depth(c, remaining - 1))
        }
        Value::Array(items) => {
            remaining == 0 || items.iter().any(|c| exceeds_depth(c, remaining - 1))
        }
        _ => false,
    }
}

/// Why a value is not a document the field's editor can load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentError {
    /// Not a JSON object whose `type` is `"doc"`.
    NotADocument,
    /// A node, mark or attribute is not shaped the way the editor reads it.
    Malformed(String),
    /// A node type the field does not enable.
    NodeNotAllowed(String),
    /// A mark type the field does not enable.
    MarkNotAllowed(String),
}

impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotADocument => f.write_str("not a rich text document"),
            Self::Malformed(detail) => write!(f, "malformed rich text document: {detail}"),
            Self::NodeNotAllowed(name) => write!(f, "node type '{name}' is not enabled"),
            Self::MarkNotAllowed(name) => write!(f, "mark type '{name}' is not enabled"),
        }
    }
}

/// One attribute of a built-in node or mark.
struct BuiltinAttr {
    name: &'static str,
    /// The editor validates the value as a number.
    numeric: bool,
}

const fn attr(name: &'static str, numeric: bool) -> BuiltinAttr {
    BuiltinAttr { name, numeric }
}

const NO_ATTRS: &[BuiltinAttr] = &[];
const HEADING_ATTRS: &[BuiltinAttr] = &[attr("level", true)];
const ORDERED_LIST_ATTRS: &[BuiltinAttr] = &[attr("order", true)];
const LINK_ATTRS: &[BuiltinAttr] = &[
    attr("href", false),
    attr("title", false),
    attr("target", false),
    attr("rel", false),
];

/// The attributes a node or mark type may carry.
enum AttrSpec<'s> {
    Builtin(&'static [BuiltinAttr]),
    /// A custom node's attr names; `None` when no registry was available to
    /// look them up (the names are then not checked).
    Custom(Option<&'s [&'s str]>),
}

/// The node and mark types one rich text field's editor is built with.
pub struct RichtextSchema<'a> {
    features: &'a [String],
    custom_nodes: HashMap<&'a str, Option<Vec<&'a str>>>,
}

impl<'a> RichtextSchema<'a> {
    /// The schema of `field`'s editor. With a registry, only registered custom
    /// nodes are enabled and their attrs are checked; without one, every name
    /// in `admin.nodes` is enabled unchecked.
    #[must_use]
    pub fn for_field(field: &'a FieldDefinition, registry: Option<&'a Registry>) -> Self {
        let custom_nodes = field
            .admin
            .nodes
            .iter()
            .filter_map(|name| {
                let Some(registry) = registry else {
                    return Some((name.as_str(), None));
                };

                let def = registry.get_richtext_node(name)?;
                let attrs = def.attrs.iter().map(|a| a.name.as_str()).collect();

                Some((name.as_str(), Some(attrs)))
            })
            .collect();

        Self {
            features: &field.admin.features,
            custom_nodes,
        }
    }

    /// Whether `feature` is enabled — every feature is when none are listed.
    #[must_use]
    pub fn has_feature(&self, feature: &str) -> bool {
        self.features.is_empty() || self.features.iter().any(|f| f == feature)
    }

    fn node_spec(&self, node_type: &str) -> Option<AttrSpec<'_>> {
        let lists = self.has_feature("orderedList") || self.has_feature("bulletList");

        let builtin = match node_type {
            "paragraph" | "hard_break" => Some(NO_ATTRS),
            "heading" => self.has_feature("heading").then_some(HEADING_ATTRS),
            "code_block" => self.has_feature("codeBlock").then_some(NO_ATTRS),
            "blockquote" => self.has_feature("blockquote").then_some(NO_ATTRS),
            "horizontal_rule" => self.has_feature("horizontalRule").then_some(NO_ATTRS),
            "bullet_list" | "list_item" => lists.then_some(NO_ATTRS),
            "ordered_list" => lists.then_some(ORDERED_LIST_ATTRS),
            _ => {
                let attrs = self.custom_nodes.get(node_type)?;
                return Some(AttrSpec::Custom(attrs.as_deref()));
            }
        };

        builtin.map(AttrSpec::Builtin)
    }

    fn mark_spec(&self, mark_type: &str) -> Option<AttrSpec<'_>> {
        let (feature, attrs) = match mark_type {
            "strong" => ("bold", NO_ATTRS),
            "em" => ("italic", NO_ATTRS),
            "code" => ("code", NO_ATTRS),
            "link" => ("link", LINK_ATTRS),
            _ => return None,
        };

        self.has_feature(feature)
            .then_some(AttrSpec::Builtin(attrs))
    }

    /// Check that `doc` is a document this field's editor can load: a `doc`
    /// root, every node and mark type enabled, attributes known and typed as
    /// the editor validates them, text nodes carrying non-empty text.
    ///
    /// # Errors
    ///
    /// Returns the first problem found, in document order.
    pub fn check(&self, doc: &Value) -> Result<(), DocumentError> {
        let root = doc.as_object().ok_or(DocumentError::NotADocument)?;

        if root.get("type").and_then(Value::as_str) != Some("doc") {
            return Err(DocumentError::NotADocument);
        }

        // The editor reads the root's marks like any node's.
        self.check_marks(root)?;
        self.check_content(root)
    }

    fn check_content(&self, node: &Map<String, Value>) -> Result<(), DocumentError> {
        let children = match node.get("content") {
            None | Some(Value::Null) => return Ok(()),
            Some(Value::Array(children)) => children,
            Some(_) => return Err(malformed("a node's content must be a list")),
        };

        children.iter().try_for_each(|child| self.check_node(child))
    }

    fn check_node(&self, value: &Value) -> Result<(), DocumentError> {
        let node = value
            .as_object()
            .ok_or_else(|| malformed("every node must be an object"))?;

        let node_type = node
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| malformed("every node must have a type"))?;

        self.check_marks(node)?;

        if node_type == "text" {
            return check_text(node);
        }

        let spec = self
            .node_spec(node_type)
            .ok_or_else(|| DocumentError::NodeNotAllowed(node_type.to_string()))?;

        check_attrs(node_type, node.get("attrs"), &spec)?;
        self.check_content(node)
    }

    fn check_marks(&self, node: &Map<String, Value>) -> Result<(), DocumentError> {
        let marks = match node.get("marks") {
            None | Some(Value::Null) => return Ok(()),
            Some(Value::Array(marks)) => marks,
            Some(_) => return Err(malformed("a node's marks must be a list")),
        };

        for mark in marks {
            let mark_type = mark
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed("every mark must have a type"))?;

            let spec = self
                .mark_spec(mark_type)
                .ok_or_else(|| DocumentError::MarkNotAllowed(mark_type.to_string()))?;

            check_attrs(mark_type, mark.get("attrs"), &spec)?;
        }

        Ok(())
    }
}

fn malformed(detail: &str) -> DocumentError {
    DocumentError::Malformed(detail.to_string())
}

/// A text node carries a non-empty string — the editor refuses empty ones.
fn check_text(node: &Map<String, Value>) -> Result<(), DocumentError> {
    match node.get("text") {
        Some(Value::String(text)) if !text.is_empty() => Ok(()),
        _ => Err(malformed("every text node must carry non-empty text")),
    }
}

/// The attributes of one node or mark: an object whose keys the type declares,
/// numeric ones holding numbers.
fn check_attrs(
    owner: &str,
    attrs: Option<&Value>,
    spec: &AttrSpec<'_>,
) -> Result<(), DocumentError> {
    let attrs = match attrs {
        None | Some(Value::Null) => return Ok(()),
        Some(Value::Object(attrs)) => attrs,
        Some(_) => {
            return Err(malformed(&format!(
                "the attrs of '{owner}' must be an object"
            )));
        }
    };

    for (name, value) in attrs {
        let numeric = match spec {
            AttrSpec::Builtin(declared) => {
                declared.iter().find(|a| a.name == name).map(|a| a.numeric)
            }
            AttrSpec::Custom(None) => Some(false),
            AttrSpec::Custom(Some(declared)) => declared.contains(&name.as_str()).then_some(false),
        };

        let Some(numeric) = numeric else {
            return Err(malformed(&format!("'{owner}' has no attribute '{name}'")));
        };

        if numeric && !value.is_number() {
            return Err(malformed(&format!(
                "attribute '{name}' of '{owner}' must be a number"
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::{FieldAdmin, FieldType, RichtextNodeDef};

    fn field(features: &[&str], nodes: &[&str]) -> FieldDefinition {
        FieldDefinition::builder("body", FieldType::Richtext)
            .admin(
                FieldAdmin::builder()
                    .richtext_format("json")
                    .features(features.iter().map(ToString::to_string).collect())
                    .nodes(nodes.iter().map(ToString::to_string).collect())
                    .build(),
            )
            .build()
    }

    fn registry_with_cta() -> Registry {
        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("cta", "CTA")
                .attrs(vec![
                    FieldDefinition::builder("url", FieldType::Text).build(),
                ])
                .build(),
        );
        reg
    }

    fn doc(content: Value) -> Value {
        let mut doc = json!({ "type": "doc" });
        doc["content"] = content;

        doc
    }

    fn para(text: &str, marks: Value) -> Value {
        let mut run = json!({ "type": "text", "text": text });
        run["marks"] = marks;

        json!({ "type": "paragraph", "content": [run] })
    }

    #[test]
    fn parse_document_accepts_text_and_objects() {
        let d = doc(json!([]));

        assert_eq!(parse_document(&d).as_deref(), Some(&d));
        assert_eq!(parse_document(&json!(d.to_string())).as_deref(), Some(&d));
    }

    #[test]
    fn parse_document_refuses_what_it_cannot_read() {
        assert!(parse_document(&json!("not json")).is_none());
        assert!(parse_document(&json!(42)).is_none());
        assert!(parse_document(&json!("[1,2]")).is_none());
        assert!(parse_document(&json!([1, 2])).is_none());

        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        let deep_doc = format!(r#"{{"type":"doc","content":{deep}}}"#);
        assert!(parse_document(&json!(deep_doc)).is_none());
    }

    #[test]
    fn all_features_enable_every_builtin_node_and_mark() {
        let f = field(&[], &[]);
        let schema = RichtextSchema::for_field(&f, None);
        let d = doc(json!([
            { "type": "heading", "attrs": { "level": 2 }, "content": [{ "type": "text", "text": "T" }] },
            para("x", json!([{ "type": "strong" }, { "type": "link", "attrs": { "href": "/a" } }])),
            { "type": "ordered_list", "attrs": { "order": 1 }, "content": [
                { "type": "list_item", "content": [para("i", json!([]))] }
            ]},
            { "type": "code_block", "content": [{ "type": "text", "text": "c" }] },
            { "type": "blockquote", "content": [para("q", json!(null))] },
            { "type": "horizontal_rule" },
            { "type": "paragraph", "content": [{ "type": "hard_break" }] },
        ]));

        assert_eq!(schema.check(&d), Ok(()));
    }

    /// A node or mark whose feature the field disables is refused — the editor
    /// could not load the document.
    #[test]
    fn disabled_features_are_refused() {
        let f = field(&["bold"], &[]);
        let schema = RichtextSchema::for_field(&f, None);

        let heading = doc(json!([{ "type": "heading", "attrs": { "level": 1 } }]));
        assert_eq!(
            schema.check(&heading),
            Err(DocumentError::NodeNotAllowed("heading".into()))
        );

        let italic = doc(json!([para("x", json!([{ "type": "em" }]))]));
        assert_eq!(
            schema.check(&italic),
            Err(DocumentError::MarkNotAllowed("em".into()))
        );

        let bold = doc(json!([para("x", json!([{ "type": "strong" }]))]));
        assert_eq!(schema.check(&bold), Ok(()));
    }

    /// Regression: the root's marks were not checked, while the editor reads
    /// them like any node's and throws on an unknown one.
    #[test]
    fn root_marks_are_checked() {
        let f = field(&["bold"], &[]);
        let schema = RichtextSchema::for_field(&f, None);

        let bad = json!({ "type": "doc", "marks": [{ "type": "em" }], "content": [] });
        assert_eq!(
            schema.check(&bad),
            Err(DocumentError::MarkNotAllowed("em".into()))
        );

        let malformed = json!({ "type": "doc", "marks": "strong" });
        assert!(matches!(
            schema.check(&malformed),
            Err(DocumentError::Malformed(_))
        ));
    }

    #[test]
    fn either_list_feature_enables_both_list_types() {
        let f = field(&["bulletList"], &[]);
        let schema = RichtextSchema::for_field(&f, None);
        let d = doc(json!([{ "type": "ordered_list", "content": [{ "type": "list_item" }] }]));

        assert_eq!(schema.check(&d), Ok(()));
    }

    /// The editor schema has no image node: a document carrying one is refused.
    #[test]
    fn unknown_and_image_nodes_are_refused() {
        let schema_field = field(&[], &[]);
        let schema = RichtextSchema::for_field(&schema_field, None);

        for node in ["image", "cta", "doc"] {
            let d = doc(json!([{ "type": node }]));
            assert_eq!(
                schema.check(&d),
                Err(DocumentError::NodeNotAllowed(node.into())),
                "{node}"
            );
        }
    }

    #[test]
    fn custom_nodes_need_registration_and_known_attrs() {
        let reg = registry_with_cta();
        let f = field(&[], &["cta", "ghost"]);
        let schema = RichtextSchema::for_field(&f, Some(&reg));

        let ok = doc(json!([{ "type": "cta", "attrs": { "url": "/x" } }]));
        assert_eq!(schema.check(&ok), Ok(()));

        let extra_attr = doc(json!([{ "type": "cta", "attrs": { "colour": "red" } }]));
        assert!(matches!(
            schema.check(&extra_attr),
            Err(DocumentError::Malformed(_))
        ));

        let unregistered = doc(json!([{ "type": "ghost" }]));
        assert_eq!(
            schema.check(&unregistered),
            Err(DocumentError::NodeNotAllowed("ghost".into()))
        );
    }

    #[test]
    fn non_documents_and_malformed_nodes_are_refused() {
        let f = field(&[], &[]);
        let schema = RichtextSchema::for_field(&f, None);

        assert_eq!(
            schema.check(&json!("hello")),
            Err(DocumentError::NotADocument)
        );
        assert_eq!(schema.check(&json!([1])), Err(DocumentError::NotADocument));
        assert_eq!(
            schema.check(&json!({ "type": "paragraph" })),
            Err(DocumentError::NotADocument)
        );

        let malformed_docs = [
            json!({ "type": "doc", "content": "x" }),
            doc(json!([1])),
            doc(json!([{ "content": [] }])),
            doc(json!([{ "type": "paragraph", "content": [{ "type": "text", "text": "" }] }])),
            doc(json!([{ "type": "heading", "attrs": { "level": "2" } }])),
            doc(json!([{ "type": "paragraph", "attrs": { "align": "left" } }])),
            doc(json!([{ "type": "paragraph", "marks": "strong" }])),
        ];

        for d in malformed_docs {
            assert!(
                matches!(schema.check(&d), Err(DocumentError::Malformed(_))),
                "{d}"
            );
        }
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

        assert!(parse_document(&deepest).is_some());
        assert!(parse_document(&json!(deepest.to_string())).is_some());
        assert!(parse_document(&too_deep).is_none());
        assert!(parse_document(&json!(too_deep.to_string())).is_none());
    }

    #[test]
    fn deeply_nested_objects_are_refused() {
        let mut doc = json!({ "type": "text" });
        for _ in 0..200 {
            doc = json!({ "type": "paragraph", "content": [doc] });
        }

        assert!(parse_document(&doc).is_none());
    }
}
