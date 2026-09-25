//! The plain text of rich text content, in either storage format.
//!
//! One extractor per format, shared by every reader that needs the words
//! rather than the markup: the search index, the `min_length` / `max_length`
//! bounds and the "is this rich text blank" predicate behind `required`. Text
//! within one block runs together exactly as displayed (a word split by a mark
//! stays one word); block boundaries separate words.

use std::collections::HashMap;

use serde_json::Value;

use crate::core::{
    FieldDefinition,
    richtext::{
        CrapNodeTag, RESERVED_NODE_NAMES, decode_entities, find_crap_nodes,
        html_lex::{Markup, Tag, raw_text_len, read_markup},
        parse_document,
    },
};

/// Custom node type → the attr names whose values count as the node's text.
pub type SearchableAttrs<'a> = HashMap<&'a str, Vec<&'a str>>;

/// HTML elements that flow within a line: their boundaries do not separate
/// words. Every other element starts a new block.
const INLINE_TAGS: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "cite", "code", "data", "del", "dfn", "em", "i", "ins", "kbd",
    "mark", "q", "s", "samp", "small", "span", "strong", "sub", "sup", "time", "u", "var",
];

/// Accumulates text block by block.
#[derive(Default)]
struct TextCollector {
    blocks: Vec<String>,
    current: String,
}

impl TextCollector {
    fn push_text(&mut self, text: &str) {
        self.current.push_str(text);
    }

    /// End the current block; blank blocks are dropped.
    fn break_block(&mut self) {
        let block = self.current.trim();

        if !block.is_empty() {
            self.blocks.push(block.to_string());
        }

        self.current.clear();
    }

    /// A value that stands as its own block (a custom node's searchable attr).
    fn push_block(&mut self, text: &str) {
        self.break_block();
        self.push_text(text);
        self.break_block();
    }

    fn finish(mut self) -> String {
        self.break_block();
        self.blocks.join(" ")
    }
}

/// The plain text of a `ProseMirror` document, including the opted-in attrs of
/// custom nodes named in `searchable`.
#[must_use]
pub fn document_text(doc: &Value, searchable: &SearchableAttrs<'_>) -> String {
    let mut out = TextCollector::default();
    collect_document(doc, searchable, &mut out);
    out.finish()
}

fn collect_document(value: &Value, searchable: &SearchableAttrs<'_>, out: &mut TextCollector) {
    let Some(node) = value.as_object() else {
        return;
    };

    let node_type = node.get("type").and_then(Value::as_str).unwrap_or("");

    match node_type {
        "text" => {
            out.push_text(node.get("text").and_then(Value::as_str).unwrap_or(""));
            return;
        }
        "hard_break" => {
            out.push_text(" ");
            return;
        }
        _ => out.break_block(),
    }

    if let Some(attrs) = searchable.get(node_type) {
        let values = node.get("attrs").and_then(Value::as_object);

        for name in attrs {
            if let Some(text) = values.and_then(|v| v.get(*name)).and_then(Value::as_str) {
                out.push_block(text);
            }
        }
    }

    for child in node
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        collect_document(child, searchable, out);
    }

    out.break_block();
}

/// The plain text of HTML rich text: element text content with character
/// references decoded and whitespace collapsed, plus the opted-in attrs of the
/// `<crap-node>` custom nodes named in `searchable`. Markup, attribute values
/// and comments are never text.
#[must_use]
pub fn html_text(html: &str, searchable: &SearchableAttrs<'_>) -> String {
    let nodes = find_crap_nodes(html);
    let mut next_node = nodes.iter().peekable();
    let mut out = TextCollector::default();
    let mut pos = 0;

    while pos < html.len() {
        while next_node.next_if(|n| n.start < pos).is_some() {}

        if let Some(node) = next_node.next_if(|n| n.start == pos) {
            push_node_attrs(node, searchable, &mut out);
            pos = node.end;
            continue;
        }

        pos += scan_html(&html[pos..], &mut out);
    }

    out.finish()
}

/// Consume one item at the start of `rest` — a comment, a tag or a text run —
/// and return its byte length (always at least one byte).
fn scan_html(rest: &str, out: &mut TextCollector) -> usize {
    match read_markup(rest) {
        Some(Markup::Comment(len)) => return len,
        Some(Markup::Tag(tag)) => return consume_tag(rest, &tag, out),
        None => {}
    }

    // At least the first character: a `<` that opens no tag is literal text.
    let first = rest.chars().next().map_or(1, char::len_utf8);
    let end = rest[first..].find('<').map_or(rest.len(), |i| i + first);

    push_html_text(&rest[..end], out);
    end
}

/// Apply a tag's effect on the text and return how far to advance: past the
/// tag, or — for a raw-text element — past its content too.
fn consume_tag(rest: &str, tag: &Tag, out: &mut TextCollector) -> usize {
    if tag.name == "br" {
        out.push_text(" ");
        return tag.len;
    }

    if !INLINE_TAGS.contains(&tag.name.as_str()) {
        out.break_block();
    }

    if !tag.opens_raw_text() {
        return tag.len;
    }

    tag.len + raw_text_len(&rest[tag.len..], tag)
}

/// A run of HTML text: references decoded, whitespace runs collapsed to one
/// space as the browser displays them.
fn push_html_text(raw: &str, out: &mut TextCollector) {
    let decoded = decode_entities(raw);
    let mut last_space = false;

    for ch in decoded.chars() {
        let space = ch.is_ascii_whitespace();

        if !(space && last_space) {
            out.current.push(if space { ' ' } else { ch });
        }

        last_space = space;
    }
}

fn push_node_attrs(node: &CrapNodeTag, searchable: &SearchableAttrs<'_>, out: &mut TextCollector) {
    out.break_block();

    let Some(names) = node.node_type().and_then(|t| searchable.get(t)) else {
        return;
    };

    let attrs = node.node_attrs();

    for name in names {
        if let Some(text) = attrs.get(*name).and_then(Value::as_str) {
            out.push_block(text);
        }
    }
}

/// The plain text of a rich text field's value in the field's format, or
/// `None` when the value is not readable in that format (validation reports
/// that on its own).
#[must_use]
pub fn richtext_plain_text(field: &FieldDefinition, value: &Value) -> Option<String> {
    let no_attrs = SearchableAttrs::new();

    if field.parses_json() {
        return parse_document(value).map(|doc| document_text(&doc, &no_attrs));
    }

    value.as_str().map(|html| html_text(html, &no_attrs))
}

/// Whether a rich text value is blank: no visible text and no custom node. An
/// editor emptied of its content still submits markup (`<p></p>`, or a `doc`
/// holding an empty paragraph), which this treats as the absent value it is.
///
/// A value unreadable in the field's format is not blank — validation refuses
/// it on its own terms.
#[must_use]
pub fn richtext_is_blank(field: &FieldDefinition, value: &Value) -> bool {
    match value {
        Value::Null => return true,
        Value::String(s) if s.trim().is_empty() => return true,
        _ => {}
    }

    if field.parses_json() {
        return parse_document(value).is_some_and(|doc| !document_has_content(&doc));
    }

    let Value::String(html) = value else {
        return false;
    };

    find_crap_nodes(html).is_empty() && html_text(html, &SearchableAttrs::new()).is_empty()
}

/// Whether a document holds visible text or a custom node.
fn document_has_content(value: &Value) -> bool {
    let Some(node) = value.as_object() else {
        return false;
    };

    let node_type = node.get("type").and_then(Value::as_str).unwrap_or("");

    if node_type == "text" {
        let text = node.get("text").and_then(Value::as_str).unwrap_or("");
        return !text.trim().is_empty();
    }

    if !node_type.is_empty() && !RESERVED_NODE_NAMES.contains(&node_type) {
        return true;
    }

    node.get("content")
        .and_then(Value::as_array)
        .is_some_and(|children| children.iter().any(document_has_content))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::{FieldAdmin, FieldType};

    /// Regression: a text run was read as a tag when its second character
    /// was a letter (`Hi</p>` parsed as a tag named `i`), dropping the text.
    #[test]
    fn text_is_never_read_as_a_tag() {
        assert_eq!(html_text("<p>Hi</p>", &SearchableAttrs::new()), "Hi");
    }

    fn richtext(format: &str) -> FieldDefinition {
        FieldDefinition::builder("body", FieldType::Richtext)
            .admin(FieldAdmin::builder().richtext_format(format).build())
            .build()
    }

    fn cta_searchable() -> SearchableAttrs<'static> {
        HashMap::from([("cta", vec!["text"])])
    }

    /// A word split by a mark is one word; paragraphs are separate words.
    #[test]
    fn document_text_joins_inline_text_and_separates_blocks() {
        let doc = json!({ "type": "doc", "content": [
            { "type": "paragraph", "content": [
                { "type": "text", "text": "un" },
                { "type": "text", "text": "believ", "marks": [{ "type": "strong" }] },
                { "type": "text", "text": "able" },
            ]},
            { "type": "paragraph", "content": [{ "type": "text", "text": "Second" }] },
        ]});

        assert_eq!(
            document_text(&doc, &SearchableAttrs::new()),
            "unbelievable Second"
        );
    }

    #[test]
    fn document_text_includes_only_searchable_attrs() {
        let doc = json!({ "type": "doc", "content": [
            { "type": "paragraph", "content": [{ "type": "text", "text": "Hello" }] },
            { "type": "cta", "attrs": { "text": "Click me", "url": "/go" } },
        ]});

        assert_eq!(document_text(&doc, &cta_searchable()), "Hello Click me");
    }

    #[test]
    fn html_text_reads_text_content_only() {
        let html = concat!(
            r#"<p>un<strong>believ</strong>able &amp; <a href="/x" title="nope">more</a></p>"#,
            "\n<h2>Second\n   line</h2><!-- hidden --><script>var x = 1;</script>",
            r#"<crap-node data-type="cta" data-attrs='{"text":"Click","url":"/go"}'></crap-node>"#,
            "<p>a<br>b&nbsp;c</p>",
        );

        assert_eq!(
            html_text(html, &cta_searchable()),
            "unbelievable & more Second line Click a b\u{a0}c"
        );
    }

    /// Regression: a quote outside an attribute-value start, or inside a
    /// declaration, opened a "value" that swallowed the text after the tag.
    #[test]
    fn html_text_reads_past_stray_quotes_in_markup() {
        let none = SearchableAttrs::new();

        assert_eq!(html_text("<p data-x=it's>kept</p>", &none), "kept");
        assert_eq!(html_text("<![CDATA[it's]]><p>after</p>", &none), "after");
        assert_eq!(html_text("<!DOCTYPE x><P>Upper</P>", &none), "Upper");
    }

    /// A node written inside a comment is not content.
    #[test]
    fn a_commented_out_node_is_blank() {
        let html_field = richtext("html");
        let value = json!(r#"<p></p><!-- <crap-node data-type="cta"></crap-node> -->"#);

        assert!(richtext_is_blank(&html_field, &value));
    }

    #[test]
    fn html_text_keeps_a_literal_angle_bracket() {
        assert_eq!(html_text("<p>1 < 2</p>", &SearchableAttrs::new()), "1 < 2");
    }

    #[test]
    fn plain_text_follows_the_field_format() {
        let json_field = richtext("json");
        let html_field = richtext("html");
        let doc = json!({ "type": "doc", "content": [
            { "type": "paragraph", "content": [{ "type": "text", "text": "Hi" }] },
        ]});

        assert_eq!(
            richtext_plain_text(&json_field, &doc).as_deref(),
            Some("Hi")
        );
        assert_eq!(
            richtext_plain_text(&json_field, &json!(doc.to_string())).as_deref(),
            Some("Hi")
        );
        assert_eq!(richtext_plain_text(&json_field, &json!("nope")), None);
        assert_eq!(
            richtext_plain_text(&html_field, &json!("<p>Hi</p>")).as_deref(),
            Some("Hi")
        );
        assert_eq!(richtext_plain_text(&html_field, &doc), None);
    }

    /// An emptied editor still submits markup; it is blank.
    #[test]
    fn empty_editor_output_is_blank() {
        let json_field = richtext("json");
        let html_field = richtext("html");

        for value in [
            json!({ "type": "doc", "content": [{ "type": "paragraph" }] }),
            json!(
                r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"  "}]}]}"#
            ),
            json!({ "type": "doc", "content": [{ "type": "horizontal_rule" }] }),
            json!(""),
            json!(null),
        ] {
            assert!(richtext_is_blank(&json_field, &value), "{value}");
        }

        for value in ["<p></p>", "<p> &nbsp; </p><p><br></p>", "  "] {
            assert!(richtext_is_blank(&html_field, &json!(value)), "{value}");
        }
    }

    #[test]
    fn text_or_a_custom_node_is_content() {
        let json_field = richtext("json");
        let html_field = richtext("html");

        assert!(!richtext_is_blank(
            &json_field,
            &json!({ "type": "doc", "content": [{ "type": "cta", "attrs": {} }] })
        ));
        assert!(!richtext_is_blank(
            &json_field,
            &json!({ "type": "doc", "content": [{ "type": "paragraph", "content": [
                { "type": "text", "text": "x" }
            ]}]})
        ));
        assert!(!richtext_is_blank(&html_field, &json!("<p>x</p>")));
        assert!(!richtext_is_blank(
            &html_field,
            &json!(r#"<crap-node data-type="cta" data-attrs="{}"></crap-node>"#)
        ));

        // Unreadable in the field's format: validation's business, not blank.
        assert!(!richtext_is_blank(&json_field, &json!("not a doc")));
    }
}
