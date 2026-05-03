//! `make field` — scaffold a per-field render template + Lua wrapper
//! plugin + a `<crap-*>` Web Component skeleton, all wired together.

mod generator;

pub use generator::{MakeFieldOptions, make_field};
