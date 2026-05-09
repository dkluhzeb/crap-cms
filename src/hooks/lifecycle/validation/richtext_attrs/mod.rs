//! Richtext custom-node-attr extraction, validation, and before_validate
//! transform pipeline. The three-step pipeline:
//!
//! 1. **extract** — walk ProseMirror JSON or HTML and pull every custom node
//!    instance with its attr values
//! 2. **validate** — run per-attr checks (required, length, numeric, email,
//!    select, date, custom Lua) against each instance
//! 3. **before_validate** — transform attr values in-place via per-attr Lua
//!    `before_validate` hooks, writing the result back into the field content

mod before_validate;
mod extract;
mod validate;

pub(crate) use before_validate::run_before_validate_on_node_attrs;
pub(crate) use validate::{RichtextValidationCtx, validate_richtext_node_attrs};
