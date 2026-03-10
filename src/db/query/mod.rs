//! CRUD query functions operating on `&rusqlite::Connection` (works with both plain
//! connections and transactions via `Deref`).

pub mod read;
pub mod write;
pub mod auth;
pub mod join;
pub mod populate;
pub mod filter;
pub mod global;
pub mod versions;
pub mod jobs;
pub mod images;
pub mod cursor;
pub mod fts;

mod types;
mod locale;
mod columns;
mod validation;
mod helpers;

pub use types::*;
pub use locale::{LocaleMode, LocaleContext, get_locale_select_columns};
pub use columns::get_column_names;
pub use validation::{is_valid_identifier, sanitize_locale, validate_field_name, validate_query_fields, validate_slug, get_valid_filter_paths};
pub use helpers::{apply_pagination_limits, normalize_date_value};

pub(crate) use locale::{group_locale_fields, locale_write_column};
pub(crate) use validation::validate_filter_field;
pub(crate) use helpers::coerce_value;

pub(super) use columns::collect_column_names;

pub use read::*;
pub use write::*;
pub use auth::*;
pub use join::*;
pub use populate::*;
pub use global::*;
pub use versions::*;

#[cfg(test)]
pub(crate) mod test_helpers;
