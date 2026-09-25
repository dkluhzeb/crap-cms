//! Rich text → indexable plain text.
//!
//! The words come from the shared rich text extractors
//! (`core::richtext::{document_text, html_text}`): text runs within a block
//! stay together (a word split by a mark is still one word), blocks are
//! separated, markup and attribute values are never indexed, and a custom
//! node contributes only its opted-in `searchable_attrs`.

use serde_json::Value;
use tracing::warn;

use crate::{
    core::richtext::{SearchableAttrs, document_text, html_text},
    db::query::fts::fields::RichtextFormat,
};

/// The indexable text of one stored rich text column value. A JSON-format
/// value that does not parse (stored before documents were validated) is
/// indexed as empty, and logged so it does not vanish from search silently.
pub(super) fn extract_richtext_text(
    raw: &str,
    format: RichtextFormat,
    searchable: &SearchableAttrs<'_>,
    column: &str,
) -> String {
    if format == RichtextFormat::Html {
        return html_text(raw, searchable);
    }

    match serde_json::from_str::<Value>(raw) {
        Ok(doc) => document_text(&doc, searchable),
        Err(e) => {
            warn!(
                column,
                "rich text value is not valid JSON; indexed as empty: {e}"
            );
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn json(raw: &str) -> String {
        extract_richtext_text(raw, RichtextFormat::Json, &SearchableAttrs::new(), "body")
    }

    #[test]
    fn json_simple() {
        let raw = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello world"}]}]}"#;
        assert_eq!(json(raw), "Hello world");
    }

    /// Regression: every text node was a separate token, so a word split by a
    /// mark (`un<strong>believ</strong>able`) indexed as three words.
    #[test]
    fn json_joins_text_within_a_block() {
        let raw = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello"},{"type":"text","text":" world","marks":[{"type":"strong"}]},{"type":"text","text":"wide"}]},{"type":"paragraph","content":[{"type":"text","text":"Second paragraph"}]}]}"#;
        assert_eq!(json(raw), "Hello worldwide Second paragraph");
    }

    #[test]
    fn json_empty_and_invalid() {
        assert_eq!(json(r#"{"type":"doc","content":[]}"#), "");
        assert_eq!(json("not json"), "");
        assert_eq!(json(""), "");
    }

    #[test]
    fn json_includes_only_searchable_node_attrs() {
        let raw = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Hello"}]},{"type":"cta","attrs":{"text":"Click me","url":"/go"}}]}"#;
        let searchable = HashMap::from([("cta", vec!["text"])]);

        let result = extract_richtext_text(raw, RichtextFormat::Json, &searchable, "body");
        assert_eq!(result, "Hello Click me");
    }

    /// Regression: an HTML value was indexed as-is — tag names, attribute
    /// values and every custom node attr (searchable or not) became search
    /// terms.
    #[test]
    fn html_indexes_text_content_and_searchable_attrs_only() {
        let raw = concat!(
            r#"<p class="lead">Hello <a href="https://example.com/secret">world</a></p>"#,
            r#"<crap-node data-type="cta" data-attrs='{"text":"Click","url":"/hidden"}'></crap-node>"#,
        );
        let searchable = HashMap::from([("cta", vec!["text"])]);

        let result = extract_richtext_text(raw, RichtextFormat::Html, &searchable, "body");
        assert_eq!(result, "Hello world Click");
    }
}
