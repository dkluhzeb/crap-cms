//! Collection CRUD handlers re-exports.

pub mod api;
pub mod forms;
pub mod item;
pub mod items;
pub mod list_collections;
pub mod shared;

// Re-export common handlers for the router
pub use api::save_user_settings::save_user_settings;
pub use api::search::search_collection;
pub use item::delete_action::delete_action;
pub use item::delete_confirm::delete_confirm;
pub use item::edit_form::edit_form;
pub use item::update_action::update_action;
pub use item::versions::list::list_versions_page;
pub use item::versions::restore_action::restore_version;
pub use item::versions::restore_confirm::restore_confirm;
pub use items::create_action::create_action;
pub use items::create_form::create_form;
pub use items::evaluate_conditions::evaluate_conditions;
pub use items::list::list_items;
pub use list_collections::list_collections;

// Re-export shared types for super/server
pub use super::shared::PaginationParams;
pub use api::search::SearchQuery;
pub use items::evaluate_conditions::EvaluateConditionsRequest;
