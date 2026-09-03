//! Authentication primitives: token management, password hashing, and pluggable providers.
//!
//! - `TokenProvider` trait — JWT token creation/validation (default: `JwtTokenProvider`)
//! - `PasswordProvider` trait — password hashing/verification (default: `Argon2PasswordProvider`)
//! - Free functions for backward compat and Lua API usage

/// JWT claims module + builder.
pub mod claims;
/// Email normalization for auth comparisons.
pub mod email;
/// Error types for auth operations.
pub mod errors;
/// Newtype wrapper for Argon2id password hashes.
pub mod hashed_password;
/// Newtype wrapper for JWT signing secrets.
pub mod jwt_secret;
/// Password hashing provider trait + Argon2id implementation.
pub mod password;
/// Token provider trait + JWT implementation.
pub mod token;
pub mod totp;
/// Authenticated user context for request extensions.
pub mod user;

pub use claims::{Claims, ClaimsBuilder, TokenUse};
pub use email::normalize_email;
pub use errors::ResetTokenError;
pub use hashed_password::HashedPassword;
pub use jwt_secret::JwtSecret;
pub use password::{
    Argon2PasswordProvider, PasswordProvider, SharedPasswordProvider, dummy_verify, hash_password,
    verify_password,
};
pub use token::{
    JwtTokenProvider, SharedTokenProvider, TokenProvider, create_token, validate_token,
};
pub use totp::{
    TOTP_STEP_SECS, generate_totp_secret, open_totp_secret, provisioning_uri, seal_totp_secret,
    totp_code_at, verify_totp,
};
pub use user::AuthUser;
