//! Authentication primitives: token management, password hashing, and pluggable providers.
//!
//! - `TokenProvider` trait — JWT token creation/validation (default: `JwtTokenProvider`)
//! - `PasswordProvider` trait — password hashing/verification (default: `Argon2PasswordProvider`)
//! - Free functions for backward compat and Lua API usage

/// Authenticated user context for request extensions.
pub mod auth_user;
/// JWT claims module.
pub mod claims;
/// Builder for JWT claims.
pub mod claims_builder;
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

pub use auth_user::AuthUser;
pub use claims::Claims;
pub use claims_builder::ClaimsBuilder;
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
