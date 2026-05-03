//! Axum handler functions for admin UI routes.

pub mod auth;
pub mod collections;
/// Filesystem-routed custom admin page handler.
pub mod custom_page;
/// Dashboard overview handlers.
pub mod dashboard;
/// Event-related handlers for the admin UI.
pub mod events;
pub mod field_context;
/// Shared form parsing helpers (multipart, array fields, select transforms).
pub mod forms;
/// Global document handlers (view/edit).
pub mod globals;
mod query;
pub mod shared;
pub mod static_assets;
/// File upload handlers.
pub mod uploads;
/// Shared types for validation endpoints.
pub mod validate;
