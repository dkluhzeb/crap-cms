//! Validation-only endpoints for collection items.
//!
//! These run the full before_validate → validate pipeline inside a rolled-back transaction,
//! returning JSON `{ valid: true }` or `{ valid: false, errors: { ... } }`.
//! Used by the `<crap-validate-form>` component to validate fields before uploading files.

mod helpers;
/// Handler for validating a create form.
pub mod validate_create;
/// Handler for validating an update form.
pub mod validate_update;

pub use validate_create::validate_create;
pub use validate_update::validate_update;
