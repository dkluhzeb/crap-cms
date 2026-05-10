//! Handlebars template formatter for the `crap-cms` CLI.
//!
//! The formatter walks a flat token stream from [`tokenizer`] and emits
//! a re-indented string via [`printer`]. The formatter is idempotent:
//! `format(format(x)) == format(x)` is a property test invariant.

mod format;
pub mod printer;
pub mod tokenizer;

pub use format::format;
