//! Locating `<crap-node>` elements in HTML rich text.
//!
//! Custom nodes travel through HTML rich text as
//! `<crap-node data-type="…" data-attrs="…"></crap-node>`. Validation, the
//! `before_validate` attr hooks and the renderer must all read the same node
//! out of the same markup the browser parses, so they share this one
//! tokenizer rather than searching for attribute substrings:
//!
//! - the tag name matches case-insensitively and may be followed by any
//!   whitespace, `/` or `>`;
//! - attributes are tokenized per HTML: names are lowercased, values may be
//!   double-quoted, single-quoted or unquoted, and a `data-type="…"` text inside
//!   another attribute's value is that value, not an attribute;
//! - a repeated attribute keeps its first occurrence, as the browser does;
//! - character references (`&quot;`, `&#39;`, `&#x27;`, `&amp;`, …) in values
//!   are decoded in one pass, the exact inverse of `html_escape_attr`;
//! - `<crap-node` text inside a comment, another element's attribute value or
//!   a `<script>` / `<style>` is not a node.

use serde_json::{Map, Value};

use crate::core::richtext::html_lex::markup_len;

const TAG: &str = "crap-node";
const CLOSE_TAG: &str = "</crap-node>";

/// One `<crap-node>` element found in HTML content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrapNodeTag {
    /// Byte offset of the element's `<`.
    pub start: usize,
    /// Byte offset just past the element: past `</crap-node>` when it has a
    /// closing tag, otherwise past its start tag.
    pub end: usize,
    /// Attributes in source order, names lowercased, values decoded.
    attrs: Vec<(String, String)>,
}

impl CrapNodeTag {
    /// The value of attribute `name` (lowercase), if present.
    #[must_use]
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The node type (`data-type`), if present.
    #[must_use]
    pub fn node_type(&self) -> Option<&str> {
        self.attr("data-type")
    }

    /// The node's attrs (`data-attrs`) as a JSON object; an empty object when
    /// absent or not a JSON object.
    #[must_use]
    pub fn node_attrs(&self) -> Map<String, Value> {
        self.attr("data-attrs")
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .and_then(|v| match v {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default()
    }
}

/// Every `<crap-node>` element in `html`, in document order.
#[must_use]
pub fn find_crap_nodes(html: &str) -> Vec<CrapNodeTag> {
    let mut out = Vec::new();
    let mut pos = 0;

    while let Some(rel) = html[pos..].find('<') {
        let start = pos + rel;

        // Anything else at a `<` — a comment, another element's tag (whose
        // attribute values may hold `<crap-node` text), a raw-text element — is
        // skipped whole, as the browser never reads a node inside it.
        let Some(after_name) = match_tag_name(html, start) else {
            pos = start + markup_len(&html[start..]).unwrap_or(1);
            continue;
        };

        let Some((attrs, start_tag_end)) = parse_attrs(html, after_name) else {
            // An unterminated start tag is not an element (the browser drops it).
            break;
        };

        // Stored content writes a node either as `<crap-node …/>` or with a
        // closing tag; a self-closed one must not swallow up to the next
        // node's `</crap-node>`.
        let self_closed = html.as_bytes()[..start_tag_end].ends_with(b"/>");
        let end = if self_closed {
            start_tag_end
        } else {
            find_close(html, start_tag_end).unwrap_or(start_tag_end)
        };

        out.push(CrapNodeTag { start, end, attrs });
        pos = end;
    }

    out
}

/// When `<crap-node` (any case) starts at `start` and is followed by a tag-name
/// terminator, the byte offset just past the name.
fn match_tag_name(html: &str, start: usize) -> Option<usize> {
    let name_start = start + 1;
    let name_end = name_start + TAG.len();

    let name = html.get(name_start..name_end)?;
    if !name.eq_ignore_ascii_case(TAG) {
        return None;
    }

    let next = html[name_end..].chars().next()?;
    (next.is_ascii_whitespace() || next == '/' || next == '>').then_some(name_end)
}

/// Tokenize the attributes of a start tag from `pos` (just past the tag name).
/// Returns the attributes and the offset just past the start tag's `>`, or
/// `None` when the tag never closes.
fn parse_attrs(html: &str, mut pos: usize) -> Option<(Vec<(String, String)>, usize)> {
    let bytes = html.as_bytes();
    let mut attrs: Vec<(String, String)> = Vec::new();

    loop {
        pos = skip_whitespace(bytes, pos);

        match *bytes.get(pos)? {
            b'>' => return Some((attrs, pos + 1)),
            b'/' => {
                pos += 1;
                continue;
            }
            _ => {}
        }

        let (name, after_name) = read_name(html, pos);
        pos = skip_whitespace(bytes, after_name);

        let value = if bytes.get(pos) == Some(&b'=') {
            let (value, after_value) = read_value(html, skip_whitespace(bytes, pos + 1))?;
            pos = after_value;
            value
        } else {
            String::new()
        };

        if !attrs.iter().any(|(n, _)| *n == name) {
            attrs.push((name, value));
        }
    }
}

fn skip_whitespace(bytes: &[u8], mut pos: usize) -> usize {
    while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
        pos += 1;
    }
    pos
}

/// An attribute name (lowercased) starting at `pos`; at least one character is
/// consumed so a stray `=` cannot stall the tokenizer.
fn read_name(html: &str, pos: usize) -> (String, usize) {
    let bytes = html.as_bytes();
    let mut end = pos + 1;

    while let Some(&b) = bytes.get(end) {
        if b.is_ascii_whitespace() || matches!(b, b'=' | b'>' | b'/') {
            break;
        }
        end += 1;
    }

    let end = next_char_boundary(html, end);
    (html[pos..end].to_ascii_lowercase(), end)
}

/// An attribute value starting at `pos`: quoted (either quote) or unquoted.
/// `None` when a quoted value never closes.
fn read_value(html: &str, pos: usize) -> Option<(String, usize)> {
    let bytes = html.as_bytes();

    if let Some(&quote) = bytes.get(pos).filter(|b| matches!(**b, b'"' | b'\'')) {
        let close = html[pos + 1..].find(quote as char)? + pos + 1;
        return Some((decode_entities(&html[pos + 1..close]), close + 1));
    }

    let mut end = pos;
    while let Some(&b) = bytes.get(end) {
        if b.is_ascii_whitespace() || b == b'>' {
            break;
        }
        end += 1;
    }

    Some((decode_entities(&html[pos..end]), end))
}

fn next_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i.min(s.len())
}

/// Offset just past the `</crap-node>` (any case) that closes an element whose
/// start tag ended at `from`.
fn find_close(html: &str, from: usize) -> Option<usize> {
    html.as_bytes()[from..]
        .windows(CLOSE_TAG.len())
        .position(|w| w.eq_ignore_ascii_case(CLOSE_TAG.as_bytes()))
        .map(|i| from + i + CLOSE_TAG.len())
}

/// Decode HTML character references in one pass: the named `&amp;`, `&lt;`,
/// `&gt;`, `&quot;`, `&apos;`, `&nbsp;` and numeric `&#NN;` / `&#xHH;`. An unknown or
/// malformed reference stays as written.
#[must_use]
pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;

    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];

        let Some((ch, used)) = decode_one(rest) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };

        out.push(ch);
        rest = &rest[used..];
    }

    out.push_str(rest);
    out
}

/// The character a reference at the start of `s` spells and its byte length.
fn decode_one(s: &str) -> Option<(char, usize)> {
    let semi = s.bytes().take(12).position(|b| b == b';')?;
    let body = &s[1..semi];

    let ch = match body {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        _ => {
            let num = body.strip_prefix('#')?;
            let code = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => num.parse::<u32>().ok()?,
            };
            char::from_u32(code)?
        }
    };

    Some((ch, semi + 1))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn types(html: &str) -> Vec<Option<String>> {
        find_crap_nodes(html)
            .iter()
            .map(|t| t.node_type().map(str::to_string))
            .collect()
    }

    #[test]
    fn finds_both_quote_styles_and_self_closing_tags() {
        let html = concat!(
            r#"<p>a</p><crap-node data-type="cta" data-attrs='{"x":1}'></crap-node>"#,
            r#"<crap-node data-type='b' data-attrs="{}" />"#,
        );

        assert_eq!(types(html), vec![Some("cta".into()), Some("b".into())]);
        assert_eq!(
            find_crap_nodes(html)[0].node_attrs().get("x"),
            Some(&json!(1))
        );
    }

    #[test]
    fn a_self_closed_node_does_not_swallow_the_next_one() {
        let html = r#"<crap-node data-type="a"/><p>x</p><crap-node data-type="b"></crap-node>"#;
        let tags = find_crap_nodes(html);

        assert_eq!(
            &html[tags[0].start..tags[0].end],
            r#"<crap-node data-type="a"/>"#
        );
        assert_eq!(tags[1].node_type(), Some("b"));
    }

    #[test]
    fn element_span_covers_the_closing_tag() {
        let html = r#"x<crap-node data-type="a"></crap-node>y"#;
        let tag = &find_crap_nodes(html)[0];

        assert_eq!(
            &html[tag.start..tag.end],
            r#"<crap-node data-type="a"></crap-node>"#
        );
    }

    /// A `data-type="…"` text inside another attribute's value is not the
    /// node's type — a substring search took the decoy.
    #[test]
    fn decoy_attribute_text_inside_a_value_is_ignored() {
        let html = r#"<crap-node data-x='data-type="decoy" data-attrs="{}"' data-type="cta" data-attrs='{"text":""}'></crap-node>"#;
        let tag = &find_crap_nodes(html)[0];

        assert_eq!(tag.node_type(), Some("cta"));
        assert_eq!(tag.node_attrs().get("text"), Some(&json!("")));
    }

    #[test]
    fn whitespace_variants_and_uppercase_tags_are_found() {
        let html = "<CRAP-NODE\n  DATA-TYPE = \"cta\"\tdata-attrs='{}'></Crap-Node><crap-node\tdata-type=cta></crap-node>";

        assert_eq!(types(html), vec![Some("cta".into()), Some("cta".into())]);
    }

    #[test]
    fn other_tags_sharing_the_prefix_are_not_nodes() {
        assert!(find_crap_nodes("<crap-nodes data-type=\"x\"></crap-nodes>").is_empty());
        assert!(find_crap_nodes("<p>no nodes</p>").is_empty());
    }

    /// The editor serializes attrs through the DOM, which entity-encodes the
    /// JSON's double quotes.
    #[test]
    fn entity_encoded_attrs_decode() {
        let html = r#"<crap-node data-type="cta" data-attrs="{&quot;text&quot;:&quot;a &amp;lt; b&quot;}"></crap-node>"#;
        let tag = &find_crap_nodes(html)[0];

        assert_eq!(tag.node_attrs().get("text"), Some(&json!("a &lt; b")));
    }

    #[test]
    fn the_first_of_a_repeated_attribute_wins() {
        let html = r#"<crap-node data-type="first" data-type="second"></crap-node>"#;

        assert_eq!(types(html), vec![Some("first".into())]);
    }

    #[test]
    fn a_greater_than_inside_a_quoted_value_does_not_end_the_tag() {
        let html = r#"<crap-node data-attrs='{"t":"a/>b"}' data-type="cta"></crap-node>"#;

        assert_eq!(types(html), vec![Some("cta".into())]);
    }

    /// Regression: `<crap-node` text inside a comment, another element's
    /// attribute value or a script was read as a node — and one inside an
    /// attribute ran to the next real node's closing tag, hiding that node
    /// from validation.
    #[test]
    fn node_text_inside_other_markup_is_not_a_node() {
        let real = r#"<crap-node data-type="real"></crap-node>"#;

        for decoy in [
            r#"<!-- <crap-node data-type="c"></crap-node> -->"#,
            r#"<p title='<crap-node data-type="a">'>x</p>"#,
            r#"<script>'<crap-node data-type="s">'</script>"#,
        ] {
            let html = format!("{decoy}{real}");
            assert_eq!(types(&html), vec![Some("real".into())], "{decoy}");
        }
    }

    #[test]
    fn an_unterminated_start_tag_is_not_a_node() {
        assert!(find_crap_nodes(r#"<crap-node data-type="cta""#).is_empty());
    }

    #[test]
    fn decode_entities_handles_named_and_numeric_references() {
        assert_eq!(decode_entities("&#39;&#x27;&quot;&lt;&gt;&amp;"), "''\"<>&");
        assert_eq!(decode_entities("&amp;lt;"), "&lt;");
        assert_eq!(decode_entities("a & b &bogus;"), "a & b &bogus;");
        assert_eq!(decode_entities("日本&amp;語"), "日本&語");
    }

    #[test]
    fn multibyte_names_and_values_do_not_panic() {
        let html = "<crap-node dätä=\"日本\" data-type=\"ノード\"></crap-node>";

        assert_eq!(types(html), vec![Some("ノード".into())]);
    }
}
