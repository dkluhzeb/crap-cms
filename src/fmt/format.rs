//! Public entry point for the formatter — wires the tokenizer to the printer.

use anyhow::Result;

use super::{printer, tokenizer};

/// Format a Handlebars template source. See [crate-level docs](super)
/// for the rule set.
///
/// # Errors
///
/// Returns an error if tokenization or pretty-printing fails (typically a
/// malformed template that the tokenizer can't recover from).
pub fn format(src: &str) -> Result<String> {
    let tokens = tokenizer::tokenize(src)?;
    printer::print(&tokens)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        /// Property: the formatter never panics on arbitrary input — a
        /// malformed template must surface as `Err`, never a crash (it runs in
        /// the pre-commit hook and CI on whatever is on disk).
        #[test]
        fn format_never_panics_on_arbitrary_input(s in any::<String>()) {
            let _ = format(&s);
        }

        /// Property: formatting is idempotent — once a template formats
        /// successfully, re-formatting its output is a fixed point. A
        /// non-idempotent formatter would thrash `fmt --check` in CI.
        #[test]
        fn format_is_idempotent_on_success(s in any::<String>()) {
            if let Ok(once) = format(&s) {
                let twice = format(&once).expect("formatted output must re-format");
                prop_assert_eq!(once, twice);
            }
        }

        /// Property: formatting never adds, drops, or reorders CONTENT — the
        /// text nodes, raw bodies, comments, and mustache expressions come
        /// through unchanged (down to whitespace, which the formatter may move
        /// and mustache-normalisation may trim). Idempotency alone can't catch
        /// a tokenizer bug that stably mangles content; this can.
        #[test]
        fn format_preserves_content(s in any::<String>()) {
            if let Ok(out) = format(&s)
                && let (Some(before), Some(after)) =
                    (content_signature(&s), content_signature(&out))
            {
                prop_assert_eq!(before, after);
            }
        }
    }

    /// The content a formatter must preserve verbatim (modulo whitespace):
    /// text, raw bodies, comments, and whitespace-normalised mustaches. HTML
    /// tag/attribute *structure* is excluded — the formatter legitimately
    /// rewrites it (void self-close, quote choice, boolean-attr and case
    /// normalisation), so it isn't content in this sense.
    fn content_signature(src: &str) -> Option<String> {
        use crate::fmt::tokenizer::{Token, tokenize};

        let mut sig = String::new();
        for tok in tokenize(src).ok()? {
            let text = match tok {
                Token::Text(s)
                | Token::RawText(s)
                | Token::RawBlock(s)
                | Token::HtmlComment(s)
                | Token::HbsComment(s)
                | Token::HbsExpr(s)
                | Token::HbsBlockOpen(s)
                | Token::HbsBlockClose(s)
                | Token::HbsElse(s)
                | Token::HbsPartialOpen(s)
                | Token::HbsPartialClose(s) => s,
                Token::HtmlStart { .. } | Token::HtmlEnd { .. } => continue,
            };
            sig.extend(text.chars().filter(|c| !c.is_whitespace()));
        }
        Some(sig)
    }

    /// The formatter is documented as idempotent (CI gates on
    /// `crap-cms fmt --check`): formatting already-formatted output must be a
    /// fixed point. This guards the tokenizer↔printer round-trip end to end.
    #[test]
    fn formatting_is_idempotent() {
        let src = "<div class=\"a\" id=\"b\">\n{{#if x}}\n<span>{{t \"hi\"}}</span>\n{{/if}}\n<input>\n</div>\n";
        let once = format(src).unwrap();
        let twice = format(&once).unwrap();
        assert_eq!(once, twice, "second format pass changed the output");
    }

    #[test]
    fn self_closes_void_elements() {
        let out = format("<input>\n").unwrap();
        assert!(out.contains("<input />"), "got: {out:?}");
    }
}
