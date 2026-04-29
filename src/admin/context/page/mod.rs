//! Typed per-page template context.
//!
//! Each admin page renders a typed [`BasePageContext`] (or
//! [`AuthBasePageContext`] for the auth flow) flattened together with a
//! page-specific struct. The combined struct serializes to JSON at the
//! `before_render` Lua-hook seam, then flows into the Handlebars renderer.
//!
//! Module layout:
//!
//! - [`types`] — page-type discriminants and breadcrumb primitives.
//! - [`meta`] — the `page` object metadata (type, title, breadcrumbs).
//! - [`base`] — full and auth-only base contexts, with builder constructors.
//!
//! Page-specific structs live in sibling modules (`auth`, `errors`,
//! `dashboard`, `collections`, `globals`) and flatten the appropriate base
//! via `#[serde(flatten)]`.

pub mod auth;
pub mod collections;
pub mod dashboard;
pub mod errors;
pub mod globals;

mod base;
mod meta;
mod types;

pub use base::{AuthBasePageContext, BasePageContext};
pub use meta::PageMeta;
pub use types::{Breadcrumb, PageType};
