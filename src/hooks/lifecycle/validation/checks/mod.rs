//! Individual validation check functions.
//! Each function performs a single validation concern and pushes errors into the provided vec.

mod condition;
mod custom;
mod date;
mod email;
mod has_many;
mod length;
mod numeric;
mod option;
mod required;
mod row_bounds;
mod unique;

pub use self::condition::evaluate_condition_table;
pub(crate) use self::custom::check_custom_validate;
pub(crate) use self::date::{check_date_field, is_valid_date_format};
pub(crate) use self::email::check_email_format;
pub use self::email::is_valid_email_format;
pub(crate) use self::has_many::check_has_many_elements;
pub(crate) use self::length::check_length_bounds;
pub(crate) use self::numeric::check_numeric_bounds;
pub(crate) use self::option::check_option_valid;
pub(crate) use self::required::check_required;
pub(crate) use self::row_bounds::check_row_bounds;
pub(crate) use self::unique::check_unique;
