//! Handlebars template formatter for `crap-cms fmt`. The public API is
//! the single `format(src) -> Result<String>` function; internal
//! pipeline is `tokenize -> print` and is `pub(crate)` so it isn't part
//! of the surface scaffolders or external crates need to see.
//!
//! ## Submodule layout
//!
//! - `tokenizer.rs` -- flat token stream from raw `.hbs` source.
//!   Recognises HTML tags, Handlebars expressions, comments, raw-content
//!   blocks (`<script>` / `<style>` / `{{{{raw}}}}`), and attribute
//!   shapes. No AST: callers read tokens linearly.
//! - `printer.rs` -- stack-based re-emit. Indent is driven by both HTML
//!   element nesting *and* Handlebars block nesting; inline collapse
//!   keeps short bodies on one line. Wide-arg helpers take typed
//!   `Emit*` input structs.
//! - `format.rs` -- the public `format` entry: `tokenize -> print`.
//!
//! ## Conventions
//!
//! - Idempotent: `format(format(x)) == format(x)` is a property-test
//!   invariant (`format::tests::format_is_idempotent_on_success`).
//! - Content-preserving: the non-whitespace character stream survives
//!   formatting unchanged (`format::tests::format_preserves_content`).
//! - Comment content (`<!-- ... -->` / `{{!-- ... --}}`) and raw blocks
//!   (`<script>`/`<style>`/`<pre>`/`<textarea>` bodies, `{{{{raw}}}}`) are
//!   preserved verbatim; the printer never reflows them.

mod format;
pub(crate) mod printer;
pub(crate) mod tokenizer;

pub use format::format;
