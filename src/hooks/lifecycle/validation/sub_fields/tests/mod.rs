//! Sub-field validation tests, split by topic to stay under the soft 1000-LOC
//! file limit. All tests exercise `validate_fields_inner` through the array
//! and blocks sub-field paths.

mod basic;
mod containers;
mod nesting;
mod value_constraints;
