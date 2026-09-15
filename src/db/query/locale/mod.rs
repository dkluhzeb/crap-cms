//! Locale types and functions for locale-aware queries.

mod context;
mod leaf_select;
mod localized_columns;
mod regroup;
mod select;
mod write;

#[cfg(test)]
mod test_support;

pub use context::{LocaleContext, LocaleMode};
pub use select::{get_locale_select_columns, get_locale_select_columns_full};

pub(crate) use context::ReadLocale;
pub(crate) use localized_columns::{column_is_localized, per_locale_columns, stored_columns};
pub(crate) use regroup::{group_locale_fields, regroup_by_locale};
pub(crate) use write::{
    is_locale_locked_write, is_non_default_single_locale, locale_locked_field_names,
    locale_write_column,
};
