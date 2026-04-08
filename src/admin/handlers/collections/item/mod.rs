//! Collection item individual handlers.

/// Handler for lazy-loading back-references on the delete page.
pub mod back_references;
/// Handler for deleting a collection item.
pub mod delete_action;
/// Handler for the delete confirmation page.
pub mod delete_confirm;
/// Handler for the collection item edit form.
pub mod edit_form;
/// Handler for undeleting a soft-deleted collection item.
pub mod undelete_action;
/// Handler for updating a collection item.
pub mod update_action;
/// Handlers for collection item versions.
pub mod versions;
