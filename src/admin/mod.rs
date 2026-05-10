//! Admin UI: Axum server, Handlebars templates, and HTMX-powered handlers.
//!
//! ## Architecture
//!
//! - **`server`** — Axum router setup, request middleware, server lifecycle.
//!   Routes are wired up in [`server::build_router`].
//! - **`server_builder`** — typed builder for the parameter bundle that
//!   [`server::start`] consumes from `commands/serve`.
//! - **`auth_middleware`** — JWT session validation (cookie-based), custom
//!   auth-strategy fallback, and admin-access gate (Lua hook). Wraps
//!   `next.run(request)` for the rest of the admin routes.
//! - **`handlers`** — one route handler per file, organized by feature
//!   (`auth/`, `collections/`, `globals/`, `events/`, `uploads/`, …). Shared
//!   helpers under `handlers::shared` (response builders, paths, paginations,
//!   access checks) and `handlers::field_context` (template context for the
//!   collection edit form).
//! - **`templates`** — Handlebars registry + custom helpers (`{{t key}}`,
//!   `{{render_field}}`, etc.). Templates are looked up from the config dir
//!   first, embedded defaults second.
//! - **`context`** — typed page-context structs that page handlers populate
//!   and serialize to JSON for Handlebars rendering.
//! - **`custom_pages`** — discovered `<config_dir>/templates/pages/<slug>.hbs`
//!   files, surfaced as `/admin/p/<slug>` routes.
//! - **`translations`** — i18n string table (English + German baseline).
//! - **`mcp_handler`** — MCP HTTP transport bolted onto the admin port for
//!   convenience; reads its own API key from `mcp.api_key`.
//!
//! ## Cross-module conventions
//!
//! - **Cookies**: names are `pub(in crate::admin)` constants in
//!   [`handlers::auth::session`] (`SESSION_COOKIE`, `MFA_PENDING_COOKIE`,
//!   `CSRF_COOKIE`, `EDITOR_LOCALE_COOKIE`, `SESSION_EXP_COOKIE`). Never
//!   spell the literal string at a call site.
//! - **URLs**: the `/admin/...` paths come from typed builders in
//!   [`handlers::shared::paths`] (`paths::login()`, `paths::collection(slug)`,
//!   …). Constants for static paths (`paths::LOGIN`,
//!   `paths::COLLECTIONS_ROOT`); functions for slug/id-bearing paths.
//! - **Errors**: handlers convert `Result<_, ServiceError>` via
//!   [`handlers::shared::service_error_to_admin_response`] (HTML 403 / 500
//!   pages) and `JoinError` via
//!   [`handlers::shared::task_join_error_response`].
//! - **Spawn-blocking**: per CLAUDE.md, the closure body is a single named
//!   fn call. Captures get bundled into a typed `*BlockingInput` struct.
//!
//! ## State
//!
//! [`AdminState`] is the application-wide bundle handed to every handler via
//! `State<AdminState>`. It is `Clone` (cheap — Arcs and small handles). Helper
//! methods like [`AdminState::email_context`] and [`AdminState::mcp_server`]
//! bundle subsets of fields for downstream consumers without exposing the
//! struct's internals. Definition lives in the sibling [`state`] module so
//! `mod.rs` stays a thin declarations + re-exports file per CLAUDE.md.

mod auth_middleware;
pub(crate) mod context;
mod csp_nonce;
pub mod custom_pages;
pub mod handlers;
mod mcp_handler;
pub mod server;
pub(crate) mod server_builder;
mod state;
pub mod templates;
pub mod translations;

pub use csp_nonce::{CSP_NONCE, CspNonce, current_nonce_or_empty};
pub use state::AdminState;
pub use translations::Translations;

// Workspace-split prep: surface cross-crate helpers at the admin root so
// callers in `api::upload` and `service::upload` import via
// `crate::admin::Foo` instead of reaching into `admin::handlers::Foo`.
// Stays `pub(crate)` because both consumers live in this crate; a future
// workspace split would promote to `pub`.
pub(crate) use handlers::{extract_join_data_from_form, parse_multipart_form};
