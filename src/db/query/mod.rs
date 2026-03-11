//! CRUD query functions operating on `&rusqlite::Connection` (works with both plain
//! connections and transactions via `Deref`).

pub mod auth;
pub mod cursor;
pub mod filter;
pub mod fts;
pub mod global;
pub mod images;
pub mod jobs;
pub mod join;
pub mod populate;
pub mod read;
pub mod versions;
pub mod write;

mod columns;
mod helpers;
mod locale;
mod types;
mod validation;

pub use columns::get_column_names;
pub use helpers::{apply_pagination_limits, normalize_date_value};
pub use locale::{LocaleContext, LocaleMode, get_locale_select_columns};
pub use types::*;
pub use validation::{
    get_valid_filter_paths, is_valid_identifier, sanitize_locale, validate_field_name,
    validate_query_fields, validate_slug,
};

pub(crate) use helpers::coerce_value;
pub(crate) use locale::{group_locale_fields, locale_write_column};
pub(crate) use validation::validate_filter_field;

pub(super) use columns::collect_column_names;

pub use auth::*;
pub use global::*;
pub use join::*;
pub use populate::*;
pub use read::*;
pub use versions::*;
pub use write::*;

#[cfg(test)]
pub(crate) mod test_helpers;
