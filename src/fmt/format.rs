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
    use super::*;

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
