//! Sub-field validation within array/blocks rows.

mod validate;

pub(super) use validate::{SubFieldParams, validate_sub_fields_inner};

#[cfg(all(test, feature = "sqlite"))]
mod tests;
