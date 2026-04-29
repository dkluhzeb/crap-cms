//! Typed template context for admin pages.
//!
//! Every admin page receives a structured context built through
//! [`ContextBuilder`](crate::admin::ContextBuilder). Each leaf shape (the
//! `crap` block, nav, document, pagination, …) is a typed `#[derive(Serialize)]`
//! struct so that renames are caught at compile time. The builder serializes
//! these structs to `serde_json::Value` at the seam to preserve the existing
//! Lua `before_render` hook contract.

mod collection;
mod crap;
mod document;
mod editor_locale;
pub mod field;
mod fields_meta;
mod global;
mod nav;
mod page;
mod pagination;
mod user;

pub use collection::{AdminMeta, AuthMeta, CollectionContext, UploadMeta, VersionsMeta};
pub use crap::CrapMeta;
pub use document::DocumentRef;
pub use editor_locale::{EditorLocaleContext, EditorLocaleOption};
pub use field::FieldContext;
pub use fields_meta::{FieldAdminMeta, FieldMeta};
pub use global::GlobalContext;
pub use nav::{NavCollection, NavData, NavGlobal};
pub use page::{Breadcrumb, PageType};
pub use pagination::PaginationContext;
pub use user::UserContext;

pub use crate::admin::ContextBuilder;
