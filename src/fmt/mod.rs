//! Handlebars template formatter for the `crap-cms` CLI.
//!
//! The formatter walks a flat token stream from [`tokenizer`] and emits
//! a re-indented string via [`printer`]. The formatter is idempotent:
//! `format(format(x)) == format(x)` is a property test invariant.

pub mod printer;
pub mod tokenizer;

use anyhow::Result;

/// Format a Handlebars template source. See [crate-level docs](self)
/// for the rule set.
pub fn format(src: &str) -> Result<String> {
    let tokens = tokenizer::tokenize(src)?;
    printer::print(&tokens)
}
