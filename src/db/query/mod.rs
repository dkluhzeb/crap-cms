//! CRUD query functions operating on `&dyn DbConnection`.

pub mod auth;
pub mod cursor;
pub mod filter;
pub mod find_pagination;
pub mod fts;
pub mod global;
pub mod images;
pub mod jobs;
pub mod join;
pub mod pagination_result;
pub mod populate;
pub mod read;
pub mod ref_count;
pub mod user_settings;
pub mod versions;
pub mod write;

mod columns;
pub(crate) mod helpers;
mod locale;
mod types;
mod validation;

pub use columns::{get_column_names, get_expected_column_names};
pub use cursor::SortDirection;
pub use find_pagination::{FindPagination, PaginationCtx, validate_find_pagination};
pub use helpers::{apply_pagination_limits, normalize_date_value};
pub use locale::{
    LocaleContext, LocaleMode, get_locale_select_columns, get_locale_select_columns_full,
};
pub use pagination_result::{PaginationResult, PaginationResultBuilder, resolve_sort};
pub use types::*;
pub use validation::{
    get_valid_filter_paths, is_valid_identifier, reject_system_field, sanitize_locale,
    validate_field_name, validate_query_fields, validate_slug,
};

#[allow(unused_imports)]
pub(crate) use helpers::{coerce_json_value, coerce_value};
pub(crate) use locale::{group_locale_fields, locale_write_column};

pub(super) use columns::collect_column_names;

pub use auth::*;
pub use global::*;
pub use join::*;
pub use populate::*;
pub use read::*;
pub use user_settings::*;
pub use versions::*;
pub use write::*;

#[cfg(test)]
pub(crate) mod test_helpers;
