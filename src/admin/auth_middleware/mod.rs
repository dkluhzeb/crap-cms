//! Admin auth machinery: per-request principal resolution
//! (`middleware`), `admin.access` gating (`gate`), error-page
//! rendering for the gate's denial paths (`pages`), and the
//! claims-to-AuthUser helper used by MFA / login-callback flows
//! (`load_user`).
//!
//! Per CLAUDE.md `mod.rs` files contain no business logic; this
//! file is module declarations and public re-exports only.

mod gate;
mod load_user;
mod middleware;
mod pages;

pub(crate) use gate::check_admin_gate_for_doc;
pub(crate) use load_user::load_auth_user;
pub(in crate::admin) use middleware::auth_middleware;
