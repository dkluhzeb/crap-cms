//! Global CRUD handlers.

/// Handler for the global edit form.
pub mod edit_form;
/// Handler for evaluating field display conditions.
pub mod evaluate_conditions;
/// Handler for updating a global.
pub mod update_action;
/// Validation-only endpoint for globals.
pub mod validate;
/// Handlers for global versions.
pub mod versions;

pub use edit_form::edit_form;
pub use evaluate_conditions::evaluate_conditions;
pub use update_action::update_action;
pub use versions::list::list_versions_page;
pub use versions::restore_action::restore_version;
pub use versions::restore_confirm::restore_confirm;
