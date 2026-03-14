//! Axum handler functions for admin UI routes.

pub mod auth;
pub mod collections;
/// Dashboard overview handlers.
pub mod dashboard;
/// Event-related handlers for the admin UI.
pub mod events;
pub mod field_context;
/// Global document handlers (view/edit).
pub mod globals;
mod query_utils;
pub mod shared;
pub mod static_assets;
/// File upload handlers.
pub mod uploads;
/// Shared types for validation endpoints.
pub mod validate;
