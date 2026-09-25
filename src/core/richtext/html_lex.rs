//! The markup items of HTML rich text, read the way the browser's tokenizer
//! reads them: comments, declarations and tags (with quoted attribute values
//! that may hold `>`), and the raw text of `<script>` / `<style>`.
//!
//! Shared by the plain-text extractor and the `<crap-node>` finder, so text,
//! blankness, node validation and rendering all agree on what is markup.

/// Elements whose content is raw text, never markup or rich text.
const RAW_TEXT_TAGS: &[&str] = &["script", "style"];

/// A tag at the start of HTML text.
pub(super) struct Tag {
    /// Lowercased element name.
    pub(super) name: String,
    pub(super) closing: bool,
    /// Byte length of the tag, through its `>` (the whole input when the tag
    /// never closes — the browser drops such a tag with the rest).
    pub(super) len: usize,
}

impl Tag {
    /// Whether this start tag opens a raw-text element.
    pub(super) fn opens_raw_text(&self) -> bool {
        !self.closing && RAW_TEXT_TAGS.contains(&self.name.as_str())
    }
}

/// One markup item at the start of HTML text.
pub(super) enum Markup {
    /// A comment, declaration (`<!…>`, `<?…>`) or bogus end tag (`</` not
    /// followed by a letter): never text. Holds its byte length.
    Comment(usize),
    Tag(Tag),
}

/// The markup item `rest` starts with, or `None` when its first character is
/// text (including a `<` that opens nothing, as in `1 < 2`).
pub(super) fn read_markup(rest: &str) -> Option<Markup> {
    let bytes = rest.as_bytes();
    if bytes.first() != Some(&b'<') {
        return None;
    }

    if rest.starts_with("<!--") {
        return Some(Markup::Comment(comment_len(rest)));
    }

    let closing = bytes.get(1) == Some(&b'/');
    let name_start = if closing { 2 } else { 1 };
    let first = *bytes.get(name_start)?;

    if first.is_ascii_alphabetic() {
        return Some(Markup::Tag(read_tag(rest, name_start, closing)));
    }

    // `<!…>` / `<?…>` declarations and a `</` not followed by a letter are
    // bogus comments: they end at the first `>`, quotes notwithstanding.
    let bogus = closing || matches!(first, b'!' | b'?');
    bogus.then(|| Markup::Comment(rest.find('>').map_or(rest.len(), |i| i + 1)))
}

/// Byte length of the comment `rest` starts with. `<!-->` and `<!--->` close
/// at once, as in the browser; an unclosed comment runs to the end.
fn comment_len(rest: &str) -> usize {
    for abrupt in ["<!-->", "<!--->"] {
        if rest.starts_with(abrupt) {
            return abrupt.len();
        }
    }

    rest[4..].find("-->").map_or(rest.len(), |i| 4 + i + 3)
}

fn read_tag(rest: &str, name_start: usize, closing: bool) -> Tag {
    let bytes = rest.as_bytes();
    let name_end = bytes[name_start..]
        .iter()
        .position(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'>'))
        .map_or(bytes.len(), |i| name_start + i);

    Tag {
        name: rest[name_start..name_end].to_ascii_lowercase(),
        closing,
        len: tag_end(bytes, name_end),
    }
}

/// Offset just past the `>` closing a tag. A quote delimits an attribute value
/// only where a value starts (after `=`); anywhere else it is an ordinary
/// character, as in the browser.
fn tag_end(bytes: &[u8], from: usize) -> usize {
    let mut quote: Option<u8> = None;
    let mut after_equals = false;

    for (i, &b) in bytes.iter().enumerate().skip(from) {
        if let Some(q) = quote {
            if b == q {
                quote = None;
                after_equals = false;
            }
            continue;
        }

        match b {
            b'>' => return i + 1,
            b'"' | b'\'' if after_equals => quote = Some(b),
            _ => {}
        }

        if !b.is_ascii_whitespace() {
            after_equals = b == b'=';
        }
    }

    bytes.len()
}

/// Byte length of a raw-text element's content: up to its closing tag (any
/// case), or the rest of the input when it never closes.
pub(super) fn raw_text_len(content: &str, tag: &Tag) -> usize {
    let close = format!("</{}", tag.name);

    content
        .as_bytes()
        .windows(close.len())
        .position(|w| w.eq_ignore_ascii_case(close.as_bytes()))
        .unwrap_or(content.len())
}

/// Byte length of the markup item `rest` starts with — a comment, or a tag
/// together with the content of a raw-text element it opens — or `None` when
/// `rest` starts with text.
pub(super) fn markup_len(rest: &str) -> Option<usize> {
    let tag = match read_markup(rest)? {
        Markup::Comment(len) => return Some(len),
        Markup::Tag(tag) => tag,
    };

    if !tag.opens_raw_text() {
        return Some(tag.len);
    }

    Some(tag.len + raw_text_len(&rest[tag.len..], &tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag_len(html: &str) -> usize {
        match read_markup(html) {
            Some(Markup::Tag(tag)) => tag.len,
            _ => panic!("not a tag: {html}"),
        }
    }

    fn comment(html: &str) -> Option<usize> {
        match read_markup(html) {
            Some(Markup::Comment(len)) => Some(len),
            _ => None,
        }
    }

    #[test]
    fn text_is_never_markup() {
        assert!(read_markup("Hi</p>").is_none());
        assert!(read_markup("< 2").is_none());
        assert!(read_markup("<3").is_none());
        assert_eq!(tag_len("<P CLASS=x>"), 11);
    }

    #[test]
    fn a_quoted_value_may_hold_a_greater_than() {
        assert_eq!(tag_len(r#"<a title="a>b">x"#), 15);
        assert_eq!(tag_len("<a title = 'a>b'>x"), 17);
    }

    /// Regression: a quote anywhere in a tag opened a "value", so an
    /// apostrophe in an unquoted value swallowed the text after the tag.
    #[test]
    fn a_quote_outside_a_value_start_is_a_plain_character() {
        assert_eq!(tag_len("<p data-x=it's>text"), 15);
        assert_eq!(tag_len("<p a\"b>text"), 7);
    }

    /// Regression: declarations honoured quotes, so an apostrophe in a CDATA
    /// section or a doctype swallowed the text after it.
    #[test]
    fn declarations_and_bogus_end_tags_end_at_the_first_greater_than() {
        assert_eq!(comment("<![CDATA[it's]]>text"), Some(16));
        assert_eq!(comment("<!DOCTYPE html>"), Some(15));
        assert_eq!(comment("<?xml version='1'?>"), Some(19));
        assert_eq!(comment("</>x"), Some(3));
        assert_eq!(comment("</ 3>x"), Some(5));
    }

    #[test]
    fn comments_close_like_the_browser_closes_them() {
        assert_eq!(comment("<!-- a > b -->x"), Some(14));
        assert_eq!(comment("<!-->x"), Some(5));
        assert_eq!(comment("<!--->x"), Some(6));
        assert_eq!(comment("<!-- open"), Some(9));
    }

    #[test]
    fn raw_text_elements_span_their_content() {
        let html = "<SCRIPT>var t = '<crap-node>';</script><p>";
        assert_eq!(markup_len(html), Some(html.find("</script>").unwrap()));
        assert_eq!(markup_len("<p>x"), Some(3));
        assert_eq!(markup_len("x<p>"), None);
    }

    #[test]
    fn multibyte_text_does_not_panic() {
        assert!(read_markup("ü<p>").is_none());
        assert_eq!(tag_len("<p title=\"ü>\">"), 15);
    }
}
