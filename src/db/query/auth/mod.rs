//! Auth query functions: password, tokens, account status, MFA.

mod mfa;
mod password;
mod status;
mod tokens;

pub use mfa::{set_mfa_code, verify_mfa_code};
pub use password::{find_by_email, get_password_hash, has_password, update_password};
pub use status::{
    get_session_version, is_locked, is_verified, lock_user, unlock_user, user_exists,
};
pub use tokens::{
    clear_reset_token, clear_verification_token, find_by_reset_token, find_by_verification_token,
    mark_unverified, mark_verified, set_reset_token, set_verification_token,
};
