//! Tokenize a Handlebars template into a flat event stream.
//!
//! The tokenizer recognises three top-level constructs: HTML tags
//! (`<tag …>` / `</tag>` / `<!-- … -->`), Handlebars expressions
//! (`{{…}}` / `{{{…}}}`), and plain text. Block helpers (`{{#…}}` /
//! `{{/…}}` / `{{else…}}`) are returned as distinct token kinds so the
//! printer can reason about indent without re-parsing.
//!
//! Attribute lists are returned as opaque slices (the span between
//! `<tag` and the closing `>`); the printer re-tokenises them on
//! demand via [`parse_attributes`].

use anyhow::{Result, anyhow};

/// A single lexical event in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token<'a> {
    HtmlStart {
        name: String,
        attrs_raw: &'a str,
        self_closed: bool,
    },
    HtmlEnd {
        name: String,
    },
    HtmlComment(&'a str),
    HbsBlockOpen(&'a str),
    HbsBlockClose(&'a str),
    HbsElse(&'a str),
    HbsPartialOpen(&'a str),
    HbsPartialClose(&'a str),
    HbsExpr(&'a str),
    HbsComment(&'a str),
    Text(&'a str),
    /// Body of a raw-content element (`<script>`, `<style>`, `<pre>`,
    /// `<textarea>`). Emitted verbatim by the printer — no indent
    /// normalization, no mustache parsing, no whitespace collapse.
    /// Required for elements whose content has its own grammar
    /// (JSON-in-script, JS source) that we must not reformat.
    RawText(&'a str),
}

/// Parsed attribute as the printer wants to consume it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attr {
    Plain { name: String, value: Option<String> },
    HbsBlock(String),
    HbsExpr(String),
}

/// HTML tags whose start tag never has a matching end tag.
const VOID_TAGS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// HTML tags whose body content has its own grammar (JS/CSS/preformatted
/// text) and must pass through the formatter verbatim. Reformatting
/// these would mangle JSON data islands, JS source, CSS rules, or
/// `<pre>` whitespace.
const RAW_CONTENT_TAGS: &[&str] = &["script", "style", "pre", "textarea"];

pub fn is_void(tag: &str) -> bool {
    VOID_TAGS.contains(&tag)
}

pub fn is_raw_content(tag: &str) -> bool {
    RAW_CONTENT_TAGS.contains(&tag)
}

/// Walk `src` left-to-right and emit a flat token stream.
pub fn tokenize(src: &str) -> Result<Vec<Token<'_>>> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    let mut text_start = 0;

    while i < bytes.len() {
        // Handlebars comments: {{!-- … --}} or {{! … }}
        if bytes[i..].starts_with(b"{{!") {
            flush_text(src, text_start, i, &mut out);
            let end = find_hbs_comment_end(src, i)
                .ok_or_else(|| anyhow!("unterminated handlebars comment at byte {}", i))?;
            out.push(Token::HbsComment(&src[i..end]));
            i = end;
            text_start = i;
            continue;
        }

        // Triple-stash {{{ … }}}
        if bytes[i..].starts_with(b"{{{") {
            flush_text(src, text_start, i, &mut out);
            let end = find_close(src, i + 3, b"}}}")
                .ok_or_else(|| anyhow!("unterminated {{{{{{...}}}}}} at byte {}", i))?
                + 3;
            out.push(Token::HbsExpr(&src[i..end]));
            i = end;
            text_start = i;
            continue;
        }

        // {{ … }} family
        if bytes[i..].starts_with(b"{{") {
            flush_text(src, text_start, i, &mut out);
            let end = find_close(src, i + 2, b"}}")
                .ok_or_else(|| anyhow!("unterminated {{{{...}}}} at byte {}", i))?
                + 2;
            let tok = classify_mustache(&src[i..end]);
            out.push(tok);
            i = end;
            text_start = i;
            continue;
        }

        // HTML comments
        if bytes[i..].starts_with(b"<!--") {
            flush_text(src, text_start, i, &mut out);
            let end = src[i + 4..]
                .find("-->")
                .map(|p| i + 4 + p + 3)
                .ok_or_else(|| anyhow!("unterminated <!-- … --> at byte {}", i))?;
            out.push(Token::HtmlComment(&src[i..end]));
            i = end;
            text_start = i;
            continue;
        }

        // Doctype, processing instructions, CDATA — passthrough as text.
        // (No format rules apply.)
        if bytes[i..].starts_with(b"<!") || bytes[i..].starts_with(b"<?") {
            // Scan to next '>' that isn't inside an attribute string.
            // For the simple cases we hit (DOCTYPE), this is enough.
            if let Some(p) = src[i..].find('>') {
                // No flush — keep this as part of surrounding text.
                i += p + 1;
                continue;
            }
        }

        // HTML tags — start, end, self-closed
        if bytes[i] == b'<' && i + 1 < bytes.len() && is_tag_start_char(bytes[i + 1]) {
            flush_text(src, text_start, i, &mut out);
            let (tok, next) = read_html_tag(src, i)?;

            // Raw-content elements (`<script>`, `<style>`, `<pre>`,
            // `<textarea>`) capture their body verbatim and emit the
            // matching close tag in one shot. Without this, the
            // printer would re-parse JS/CSS/JSON/preformatted text as
            // if it were Handlebars/HTML and reflow it, breaking JSON
            // string literals across newlines, collapsing JS spaces,
            // etc.
            let raw_tag = match &tok {
                Token::HtmlStart {
                    name, self_closed, ..
                } if !*self_closed && is_raw_content(name) => Some(name.clone()),
                _ => None,
            };
            if let Some(tag_name) = raw_tag {
                out.push(tok);
                let body_start = next;
                let close_pos = find_raw_close(src, body_start, &tag_name)
                    .ok_or_else(|| anyhow!("unterminated <{}> at byte {}", tag_name, i))?;
                if close_pos > body_start {
                    out.push(Token::RawText(&src[body_start..close_pos]));
                }
                let after_close = src[close_pos..]
                    .find('>')
                    .map(|p| close_pos + p + 1)
                    .ok_or_else(|| anyhow!("unterminated </{}> tag", tag_name))?;
                out.push(Token::HtmlEnd { name: tag_name });
                i = after_close;
                text_start = i;
                continue;
            }

            out.push(tok);
            i = next;
            text_start = i;
            continue;
        }

        i += 1;
    }
    flush_text(src, text_start, i, &mut out);
    Ok(out)
}

fn flush_text<'a>(src: &'a str, start: usize, end: usize, out: &mut Vec<Token<'a>>) {
    if end > start {
        out.push(Token::Text(&src[start..end]));
    }
}

fn is_tag_start_char(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'/'
}

/// Find the byte position of `needle` starting from `from`, respecting
/// quote boundaries. The returned index is the start of `needle`.
fn find_close(src: &str, from: usize, needle: &[u8]) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut i = from;
    let mut quote: Option<u8> = None;
    while i + needle.len() <= bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if b == b'"' || b == b'\'' {
            quote = Some(b);
            i += 1;
            continue;
        }
        if bytes[i..i + needle.len()] == *needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Find the end of `{{!-- … --}}` (returns index past the trailing `}}`).
fn find_hbs_comment_end(src: &str, from: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes[from..].starts_with(b"{{!--") {
        // Block comment: closes at `--}}`
        return src[from + 5..].find("--}}").map(|p| from + 5 + p + 4);
    }
    // {{! …}} (no block form): closes at `}}`
    src[from + 3..].find("}}").map(|p| from + 3 + p + 2)
}

/// Classify a mustache token (already extracted, including outer `{{`/`}}`).
fn classify_mustache(raw: &str) -> Token<'_> {
    let inner = strip_mustache(raw);
    let trimmed = inner.trim_start();
    if let Some(rest) = trimmed.strip_prefix("#>") {
        let _ = rest;
        return Token::HbsPartialOpen(raw);
    }
    if let Some(rest) = trimmed.strip_prefix('#') {
        let _ = rest;
        return Token::HbsBlockOpen(raw);
    }
    if let Some(rest) = trimmed.strip_prefix('/') {
        // Detect close-of-partial vs close-of-block by checking whether the
        // name contains a '/'. Partials use `partials/foo`; blocks use
        // bare names like `if`, `each`, `unless`.
        if rest.contains('/') {
            return Token::HbsPartialClose(raw);
        }
        return Token::HbsBlockClose(raw);
    }
    if trimmed.starts_with("else") {
        return Token::HbsElse(raw);
    }
    Token::HbsExpr(raw)
}

fn strip_mustache(raw: &str) -> &str {
    raw.trim_start_matches('{').trim_end_matches('}')
}

/// Read an HTML tag starting at `start` (where `src.as_bytes()[start] == b'<'`).
/// Returns the token and the byte index just past the tag.
fn read_html_tag(src: &str, start: usize) -> Result<(Token<'_>, usize)> {
    let bytes = src.as_bytes();
    let mut i = start + 1;
    let is_end = bytes[i] == b'/';
    if is_end {
        i += 1;
    }
    let name_start = i;
    while i < bytes.len() && is_name_char(bytes[i]) {
        i += 1;
    }
    if i == name_start {
        return Err(anyhow!("expected tag name at byte {}", start));
    }
    let name = src[name_start..i].to_ascii_lowercase();

    if is_end {
        // Skip to '>'
        while i < bytes.len() && bytes[i] != b'>' {
            i += 1;
        }
        if i >= bytes.len() {
            return Err(anyhow!("unterminated </{}> tag", name));
        }
        return Ok((Token::HtmlEnd { name }, i + 1));
    }

    // Attributes are everything between here and the final `>` (or `/>`),
    // respecting quote boundaries and braces.
    let attrs_start = i;
    let mut self_closed = false;
    let mut quote: Option<u8> = None;
    let mut hbs_depth = 0u32;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if hbs_depth > 0 {
            if bytes[i..].starts_with(b"}}") {
                hbs_depth -= 1;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if b == b'"' || b == b'\'' {
            quote = Some(b);
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"{{") {
            hbs_depth += 1;
            i += 2;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'>' {
            self_closed = true;
            let attrs_raw = src[attrs_start..i].trim_end();
            return Ok((
                Token::HtmlStart {
                    name,
                    attrs_raw,
                    self_closed,
                },
                i + 2,
            ));
        }
        if b == b'>' {
            let attrs_raw = src[attrs_start..i].trim_end();
            return Ok((
                Token::HtmlStart {
                    name,
                    attrs_raw,
                    self_closed,
                },
                i + 1,
            ));
        }
        i += 1;
    }
    Err(anyhow!("unterminated tag starting at byte {}", start))
}

fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b':'
}

/// Find the byte position of the next `</tag>` (case-insensitive) at or
/// after `from`. Returns the index of the leading `<`, or `None` if no
/// close tag is found. Used to bound the body of raw-content elements
/// (`<script>`, `<style>`, `<pre>`, `<textarea>`) — the HTML5 parser
/// terminates these on the first `</tag>` regardless of nesting, so a
/// linear scan is correct. Per-byte ASCII-lowercase comparison since
/// tag names are ASCII.
fn find_raw_close(src: &str, from: usize, tag: &str) -> Option<usize> {
    let bytes = src.as_bytes();
    let needle_len = 2 + tag.len();
    let mut i = from;
    while i + needle_len <= bytes.len() {
        if bytes[i] == b'<' && bytes[i + 1] == b'/' {
            let candidate = &bytes[i + 2..i + 2 + tag.len()];
            if candidate.eq_ignore_ascii_case(tag.as_bytes()) {
                let after = i + 2 + tag.len();
                if after < bytes.len()
                    && (bytes[after] == b'>' || bytes[after].is_ascii_whitespace())
                {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

/// Re-parse an attribute slice into individual attributes. Supports:
///   - `name="value"`, `name='value'`, `name=value`, `name`
///   - `{{#if x}}required{{/if}}` and similar embedded blocks
///   - bare `{{var}}` expressions used as attributes
pub fn parse_attributes(raw: &str) -> Result<Vec<Attr>> {
    let mut out = Vec::new();
    let bytes = raw.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Skip whitespace.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }

        // Handlebars block as attribute: {{#if …}} … {{/if}}
        if raw[i..].starts_with("{{#") || raw[i..].starts_with("{{^") {
            let end = consume_hbs_block_attr(raw, i)?;
            out.push(Attr::HbsBlock(raw[i..end].trim().to_string()));
            i = end;
            continue;
        }

        // Bare {{ … }} as an attribute (rare but legal)
        if raw[i..].starts_with("{{") {
            let close = find_close(raw, i + 2, b"}}")
                .ok_or_else(|| anyhow!("unterminated {{...}} in attribute list"))?
                + 2;
            out.push(Attr::HbsExpr(raw[i..close].to_string()));
            i = close;
            continue;
        }

        // Plain attribute: name [= value]
        let name_start = i;
        while i < bytes.len() && is_attr_name_char(bytes[i]) {
            i += 1;
        }
        if i == name_start {
            // Couldn't make progress — bail safely with whatever's left.
            return Err(anyhow!(
                "unrecognised attribute syntax at byte {} of `{}`",
                i,
                raw
            ));
        }
        let name = raw[name_start..i].to_ascii_lowercase();

        // Optional `= value`
        let mut value: Option<String> = None;
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            if i >= bytes.len() {
                return Err(anyhow!("trailing '=' in attribute list `{}`", raw));
            }
            let q = bytes[i];
            if q == b'"' || q == b'\'' {
                let close = find_attr_value_quote(raw, i + 1, q)
                    .ok_or_else(|| anyhow!("unterminated attribute value in `{}`", raw))?;
                value = Some(raw[i + 1..close].to_string());
                i = close + 1;
            } else {
                let val_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = Some(raw[val_start..i].to_string());
            }
        }
        out.push(Attr::Plain { name, value });
    }
    Ok(out)
}

/// Find the closing quote `q` for an attribute value starting at byte
/// `from`. Skips over any `{{…}}` / `{{#…}}…{{/…}}` constructs
/// (including their inner quoted strings). Returns the byte index of
/// the closing quote, or `None` if unterminated.
fn find_attr_value_quote(raw: &str, from: usize, q: u8) -> Option<usize> {
    let bytes = raw.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if raw[i..].starts_with("{{") {
            // Skip a balanced handlebars region. Use brace-depth so a
            // bare `{{x}}` and a `{{#if x}}…{{/if}}` are both consumed.
            let mut depth = 0i32;
            while i < bytes.len() {
                if raw[i..].starts_with("{{") {
                    depth += 1;
                    i += 2;
                } else if raw[i..].starts_with("}}") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if bytes[i] == q {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn is_attr_name_char(b: u8) -> bool {
    !(b.is_ascii_whitespace() || b == b'=' || b == b'>' || b == b'/' || b == b'"' || b == b'\'')
}

/// Consume a `{{#…}}…{{/…}}` block embedded inside an attribute list,
/// returning the byte index just past the closing `{{/…}}`.
fn consume_hbs_block_attr(raw: &str, start: usize) -> Result<usize> {
    let bytes = raw.as_bytes();
    let mut i = start;
    let mut depth = 0i32;
    loop {
        if i >= bytes.len() {
            return Err(anyhow!(
                "unterminated handlebars block in attribute list at byte {}",
                start
            ));
        }
        if raw[i..].starts_with("{{#") || raw[i..].starts_with("{{^") {
            depth += 1;
            let close =
                find_close(raw, i + 2, b"}}").ok_or_else(|| anyhow!("unterminated {{#...}}"))? + 2;
            i = close;
            continue;
        }
        if raw[i..].starts_with("{{/") {
            depth -= 1;
            let close =
                find_close(raw, i + 2, b"}}").ok_or_else(|| anyhow!("unterminated {{/...}}"))? + 2;
            i = close;
            if depth == 0 {
                return Ok(i);
            }
            continue;
        }
        if raw[i..].starts_with("{{") {
            let close =
                find_close(raw, i + 2, b"}}").ok_or_else(|| anyhow!("unterminated {{...}}"))? + 2;
            i = close;
            continue;
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(toks: &[Token<'_>]) -> Vec<&'static str> {
        toks.iter()
            .map(|t| match t {
                Token::HtmlStart { .. } => "Start",
                Token::HtmlEnd { .. } => "End",
                Token::HtmlComment(_) => "HtmlComment",
                Token::HbsBlockOpen(_) => "BlockOpen",
                Token::HbsBlockClose(_) => "BlockClose",
                Token::HbsElse(_) => "Else",
                Token::HbsPartialOpen(_) => "PartialOpen",
                Token::HbsPartialClose(_) => "PartialClose",
                Token::HbsExpr(_) => "Expr",
                Token::HbsComment(_) => "HbsComment",
                Token::Text(_) => "Text",
                Token::RawText(_) => "RawText",
            })
            .collect()
    }

    #[test]
    fn html_basic() {
        let toks = tokenize("<div>hello</div>").unwrap();
        assert_eq!(names(&toks), ["Start", "Text", "End"]);
    }

    #[test]
    fn handlebars_expr_and_block() {
        let toks = tokenize("{{#if x}}{{name}}{{/if}}").unwrap();
        assert_eq!(names(&toks), ["BlockOpen", "Expr", "BlockClose"]);
    }

    #[test]
    fn partial_block() {
        let toks = tokenize("{{#> partials/field}}body{{/partials/field}}").unwrap();
        assert_eq!(names(&toks), ["PartialOpen", "Text", "PartialClose"]);
    }

    #[test]
    fn else_token() {
        let toks = tokenize("{{#if x}}a{{else}}b{{/if}}").unwrap();
        assert_eq!(
            names(&toks),
            ["BlockOpen", "Text", "Else", "Text", "BlockClose"]
        );
    }

    #[test]
    fn hbs_comment_block_form() {
        let src = "{{!-- doc\nlines --}}rest";
        let toks = tokenize(src).unwrap();
        assert!(matches!(toks[0], Token::HbsComment(s) if s == "{{!-- doc\nlines --}}"));
    }

    #[test]
    fn html_comment() {
        let toks = tokenize("<!-- a -->").unwrap();
        assert!(matches!(toks[0], Token::HtmlComment("<!-- a -->")));
    }

    #[test]
    fn triple_stash() {
        let toks = tokenize("{{{render}}}").unwrap();
        assert!(matches!(toks[0], Token::HbsExpr("{{{render}}}")));
    }

    #[test]
    fn self_closed_void() {
        let toks = tokenize("<input type=\"text\" />").unwrap();
        let Token::HtmlStart {
            name, self_closed, ..
        } = &toks[0]
        else {
            panic!()
        };
        assert_eq!(name, "input");
        assert!(self_closed);
    }

    #[test]
    fn attrs_with_hbs_conditional() {
        let toks = tokenize("<input {{#if required}}required{{/if}} type=\"text\" />").unwrap();
        let Token::HtmlStart { attrs_raw, .. } = &toks[0] else {
            panic!()
        };
        let attrs = parse_attributes(attrs_raw).unwrap();
        assert_eq!(attrs.len(), 2);
        assert!(matches!(&attrs[0], Attr::HbsBlock(s) if s.contains("{{#if required}}")));
        assert!(matches!(
            &attrs[1],
            Attr::Plain { name, value }
                if name == "type" && value.as_deref() == Some("text")
        ));
    }

    #[test]
    fn attrs_boolean_and_value() {
        let attrs = parse_attributes("required disabled type=\"text\"").unwrap();
        assert_eq!(attrs.len(), 3);
        assert!(matches!(&attrs[0], Attr::Plain { name, value: None } if name == "required"));
        assert!(matches!(&attrs[1], Attr::Plain { name, value: None } if name == "disabled"));
    }

    #[test]
    fn handlebars_in_attr_value() {
        let toks = tokenize("<a href=\"/foo/{{ id }}/bar\">x</a>").unwrap();
        let Token::HtmlStart { attrs_raw, .. } = &toks[0] else {
            panic!()
        };
        let attrs = parse_attributes(attrs_raw).unwrap();
        assert_eq!(attrs.len(), 1);
        assert!(matches!(
            &attrs[0],
            Attr::Plain { name, value }
                if name == "href" && value.as_deref() == Some("/foo/{{ id }}/bar")
        ));
    }

    #[test]
    fn doctype_passthrough() {
        let toks = tokenize("<!doctype html><html></html>").unwrap();
        // Doctype is folded into surrounding text; first real token is <html>.
        assert!(matches!(&toks[0], Token::Text(s) if s.eq_ignore_ascii_case("<!doctype html>")));
    }

    #[test]
    fn script_body_captured_as_raw() {
        let src = "<script>var x={\"a\":1};</script>";
        let toks = tokenize(src).unwrap();
        assert_eq!(names(&toks), ["Start", "RawText", "End"]);
        assert!(matches!(&toks[1], Token::RawText("var x={\"a\":1};")));
    }

    #[test]
    fn script_with_mustache_in_body_does_not_tokenize_mustache() {
        let src = "<script>{{t \"hello\"}}</script>";
        let toks = tokenize(src).unwrap();
        // Body is raw — the {{...}} stays inside RawText, not split out.
        assert_eq!(names(&toks), ["Start", "RawText", "End"]);
        assert!(matches!(&toks[1], Token::RawText("{{t \"hello\"}}")));
    }

    #[test]
    fn empty_script_emits_no_raw_text() {
        let src = "<script src=\"/x.js\"></script>";
        let toks = tokenize(src).unwrap();
        assert_eq!(names(&toks), ["Start", "End"]);
    }

    #[test]
    fn style_pre_textarea_are_raw_content() {
        for tag in ["style", "pre", "textarea"] {
            let src = format!("<{tag}>x  y\n  z</{tag}>");
            let toks = tokenize(&src).unwrap();
            assert_eq!(names(&toks), ["Start", "RawText", "End"], "tag={tag}");
        }
    }

    #[test]
    fn close_tag_match_is_case_insensitive() {
        let toks = tokenize("<script>x</SCRIPT>").unwrap();
        assert_eq!(names(&toks), ["Start", "RawText", "End"]);
    }
}
