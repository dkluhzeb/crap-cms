//! Axum handler functions for admin UI routes.

pub(crate) mod auth;
pub(crate) mod collections;
/// Filesystem-routed custom admin page handler.
pub(crate) mod custom_page;
/// Dashboard overview handlers.
pub(crate) mod dashboard;
/// Event-related handlers for the admin UI.
pub(crate) mod events;
pub(crate) mod field_context;
/// Shared form parsing helpers (multipart, array fields, select transforms).
/// Stays `pub` because `parse_multipart_form` and `FormData` are re-used by
/// `api::upload` and `service::upload` respectively.
pub mod forms;
/// Global document handlers (view/edit).
pub(crate) mod globals;
mod query;
pub(crate) mod shared;
pub(crate) mod static_assets;
/// File upload handlers.
pub(crate) mod uploads;
/// Shared types for validation endpoints.
pub(crate) mod validate;

// Workspace-split prep: surface the two cross-module handlers from `forms`
// at the top of `handlers` so callers in `api::upload` and `service::upload`
// depend on `admin::handlers::Foo` rather than the deeper
// `admin::handlers::forms::Foo`. Stays `pub(crate)` because both consumers
// live in this crate; a future workspace split would promote to `pub`.
pub(crate) use forms::{FormData, parse_multipart_form};
