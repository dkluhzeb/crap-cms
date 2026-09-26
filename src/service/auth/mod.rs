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
//! - [`password_reset`] — the reset-with-token operation every surface calls
//!   (one transaction, committed only on success).
//! - [`account`] — lock / unlock / verified-flag mutation + the
//!   accessors used by middleware to decide whether to accept a
//!   token (session version, `is_locked`, `user_exists`).
//! - [`mfa`] — email / custom MFA code persistence, delivery + verification.
//! - [`mfa_gate`] — whether a verified authentication (login or auth
//!   callback) must complete the collection's second factor, and whether a
//!   session that did not satisfy it may authenticate a request.
//! - [`challenge`] — issuing the MFA challenge (throttle, surface-bound
//!   pending token, code delivery) for every login surface.
//! - [`session`] — minting a session token, stamped with its surface and
//!   second-factor state, for every login surface.
//! - [`totp_flow`] — TOTP challenge/enrollment + the mode-dispatching
//!   [`totp_flow::verify_second_factor`] chokepoint both login surfaces use.
//! - [`evaluator`] — the unified per-request auth resolver shared
//!   by admin middleware and the gRPC service.
//! - [`strategy_user`] — admitting the user a custom strategy or an
//!   external auth callback names (stored flags decide, the hook may only
//!   restrict).

pub mod account;
pub mod challenge;
pub mod evaluator;
pub mod local;
pub mod login_flow;
pub mod mfa;
pub mod mfa_gate;
pub mod password_reset;
pub mod session;
pub mod strategy_user;
pub mod tokens;
pub mod totp_flow;

#[cfg(test)]
mod test_support;

pub use account::{
    AccountAction, apply_account_action, bump_session_version, check_account_action_access,
    get_session_version, is_locked, is_verified, load_user, lock_user, mark_unverified,
    mark_verified, perform_account_action, reset_totp, set_password, unlock_user, user_exists,
};
pub use challenge::{ChallengeRefusal, ChallengeRequest, MfaChallenge, issue_mfa_challenge};
pub use evaluator::{
    AuthFailure, AuthRequest, AuthenticatedResolution, EvaluateDeps, Resolution, ResolvedMethod,
    evaluate, load_authenticated_user, reload_authenticated_user,
};
pub use local::{AuthResult, authenticate_local};
pub use login_flow::{LoginFlowRequest, LoginOutcome, LoginVerified, verify_login};
pub use mfa::{
    MFA_PENDING_EXPIRY, MfaCodeDelivery, deliver_mfa_code, generate_mfa_code, set_mfa_code,
    verify_mfa_code,
};
pub use mfa_gate::{MfaGateRequest, mfa_gate, second_factor_required};
pub use password_reset::{PasswordReset, reset_password_with_token};
pub use session::{MintedSession, SessionGrant, SessionGrantBuilder, mint_session};
pub use strategy_user::{StrategyAdmission, StrategyRefusal, admit_strategy_user};
pub use tokens::{
    ResetTokenResult, VERIFICATION_TOKEN_EXPIRY, VerificationTokenResult,
    consume_verification_token, find_by_reset_token, generate_reset_token, generate_security_token,
    generate_verification_token, issue_verification_token,
};
pub use totp_flow::{TotpProvisioning, totp_challenge, verify_second_factor};
