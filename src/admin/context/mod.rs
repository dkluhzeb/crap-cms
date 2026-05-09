//! Typed template context for admin pages.
//!
//! Every admin page constructs its specific [`page::*Page`](page) typed
//! struct, which flattens a [`BasePageContext`] (or [`AuthBasePageContext`]
//! for auth-flow pages) and adds page-specific fields. Each leaf shape (the
//! `crap` block, nav, document, pagination, …) is a typed
//! `#[derive(Serialize)]` struct, so renames are caught at compile time.
//! Page contexts are serialized to `serde_json::Value` at the
//! `before_render` Lua-hook seam, then handed to the Handlebars renderer.

mod collection;
mod crap;
mod document;
mod editor_locale;
pub mod field;
mod fields_meta;
mod global;
mod locale_template;
mod nav;
pub mod page;
mod pagination;
mod user;

pub use collection::{CollectionContext, VersionsMeta};
pub use crap::CrapMeta;
pub use document::DocumentRef;
pub use editor_locale::{EditorLocaleContext, EditorLocaleOption};
pub use field::FieldContext;
pub use fields_meta::FieldMeta;
pub use global::GlobalContext;
pub use locale_template::LocaleTemplateData;
pub use nav::NavData;
pub use page::{AuthBasePageContext, BasePageContext, Breadcrumb, PageMeta, PageType};
pub use pagination::PaginationContext;
pub use user::UserContext;
