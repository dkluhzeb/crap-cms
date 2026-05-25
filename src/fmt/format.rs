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
