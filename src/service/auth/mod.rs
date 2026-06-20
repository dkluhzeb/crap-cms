//! Auth service layer — composite authentication operations.
//!
//! Per CLAUDE.md `mod.rs` files contain no business logic — only
//! module declarations and re-exports. Each submodule owns one
//! concern:
//!
//! - [`local`] — `email + password → AuthResult` (the Login flow's
//!   credential check).
//! - [`tokens`] — password-reset + email-verification token
//!   generation / consumption.
//! - [`account`] — lock / unlock / verified-flag mutation + the
//!   accessors used by middleware to decide whether to accept a
//!   token (session version, `is_locked`, `user_exists`).
//! - [`mfa`] — email MFA code persistence + verification.
//! - [`evaluator`] — the unified per-request auth resolver shared
//!   by admin middleware and the gRPC service.

pub mod account;
pub mod evaluator;
pub mod local;
pub mod mfa;
pub mod tokens;

#[cfg(test)]
mod test_support;

pub use account::{
    AccountAction, bump_session_version, get_session_version, is_locked, is_verified, lock_user,
    mark_unverified, mark_verified, perform_account_action, unlock_user, user_exists,
};
pub use evaluator::{
    AuthFailure, AuthRequest, AuthenticatedResolution, EvaluateDeps, Resolution, ResolvedMethod,
    evaluate, load_authenticated_user,
};
pub use local::{AuthResult, authenticate_local};
pub use mfa::{set_mfa_code, verify_mfa_code};
pub use tokens::{
    ResetTokenResult, consume_reset_token, consume_verification_token, find_by_reset_token,
    generate_reset_token,
};
