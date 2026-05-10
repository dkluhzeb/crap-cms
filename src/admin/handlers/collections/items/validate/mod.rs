//! Validation-only endpoints for collection items.
//!
//! These run the full before_validate → validate pipeline inside a rolled-back transaction,
//! returning JSON `{ valid: true }` or `{ valid: false, errors: { ... } }`.
//! Used by the `<crap-validate-form>` component to validate fields before uploading files.

/// Handler for validating a create form.
pub mod create;
mod helpers;
/// Handler for validating an update form.
pub mod update;

pub use create::validate_create;
pub use update::validate_update;
