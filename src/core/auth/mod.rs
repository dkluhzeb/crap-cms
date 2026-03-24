//! Authentication primitives: Argon2id password hashing and JWT token management.

/// JWT claims module.
pub mod claims;
/// Builder for JWT claims.
pub mod claims_builder;
/// Newtype wrapper for Argon2id password hashes.
pub mod hashed_password;
/// Newtype wrapper for JWT signing secrets.
pub mod jwt_secret;

use std::{fmt, sync::LazyLock};

use anyhow::{Context as _, Result, anyhow};

/// Error types for password reset token operations.
#[derive(Debug, Clone, PartialEq)]
pub enum ResetTokenError {
    /// The token was not found in any auth collection.
    NotFound,
    /// The token was found but has expired.
    Expired,
}

impl fmt::Display for ResetTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "Invalid reset token"),
            Self::Expired => write!(f, "Reset token has expired"),
        }
    }
}

impl std::error::Error for ResetTokenError {}
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
pub use claims::Claims;
pub use claims_builder::ClaimsBuilder;
pub use hashed_password::HashedPassword;
pub use jwt_secret::JwtSecret;

/// Pre-computed Argon2 hash used to burn CPU time on user-not-found paths,
/// preventing timing oracles that leak whether an email exists.
static DUMMY_HASH: LazyLock<HashedPassword> =
    LazyLock::new(|| hash_password("__crap_dummy_timing__").expect("dummy hash"));

/// Perform a dummy password verification to equalize timing with real verifications.
/// Call this on login paths where user-not-found or hash-missing would otherwise
/// return fast, enabling email enumeration via response timing.
pub fn dummy_verify() {
    let _ = verify_password("x", DUMMY_HASH.as_ref());
}

/// Hash a password using Argon2id.
pub fn hash_password(password: &str) -> Result<HashedPassword> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow!("Password hashing failed: {}", e))?;
    Ok(HashedPassword::new(hash.to_string()))
}

/// Verify a password against a stored hash.
pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(hash).map_err(|e| anyhow!("Invalid password hash: {}", e))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// Full authenticated user context stored in request extensions.
/// Contains the JWT claims and the full user document from the database.
#[derive(Debug, Clone)]
pub struct AuthUser {
    /// The decoded JWT claims for this user.
    #[allow(dead_code)]
    pub claims: Claims,
    /// The full document representing this user from their auth collection.
    pub user_doc: crate::core::Document,
    /// Preferred admin UI locale (e.g. "en", "de"). Loaded from user settings.
    pub ui_locale: String,
}

impl AuthUser {
    /// Create a new `AuthUser` instance with the given claims and document.
    pub fn new(claims: Claims, user_doc: crate::core::Document) -> Self {
        Self {
            claims,
            user_doc,
            ui_locale: "en".to_string(),
        }
    }
}

/// Create a signed JWT token from claims.
pub fn create_token(claims: &Claims, secret: &str) -> Result<String> {
    let key = jsonwebtoken::EncodingKey::from_secret(secret.as_bytes());
    jsonwebtoken::encode(&jsonwebtoken::Header::default(), claims, &key)
        .context("Failed to create JWT token")
}

/// Validate a JWT token and return the claims.
pub fn validate_token(token: &str, secret: &str) -> Result<Claims> {
    let key = jsonwebtoken::DecodingKey::from_secret(secret.as_bytes());
    let mut validation = jsonwebtoken::Validation::default();
    validation.required_spec_claims.clear();
    validation.validate_exp = true;
    let data =
        jsonwebtoken::decode::<Claims>(token, &key, &validation).context("Invalid JWT token")?;
    Ok(data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify_password() {
        let hash = hash_password("secret123").unwrap();
        assert!(verify_password("secret123", hash.as_ref()).unwrap());
        assert!(!verify_password("wrong", hash.as_ref()).unwrap());
    }

    #[test]
    fn create_and_validate_token() {
        let claims = Claims::builder("user123", "users")
            .email("test@example.com")
            .exp((chrono::Utc::now().timestamp() as u64) + 3600)
            .build();
        let token = create_token(&claims, "test-secret").unwrap();
        let decoded = validate_token(&token, "test-secret").unwrap();
        assert_eq!(decoded.sub, "user123");
        assert_eq!(decoded.email, "test@example.com");
    }

    #[test]
    fn expired_token_fails() {
        let claims = Claims::builder("user123", "users")
            .email("test@example.com")
            .exp(0) // expired
            .build();
        let token = create_token(&claims, "test-secret").unwrap();
        assert!(validate_token(&token, "test-secret").is_err());
    }

    #[test]
    fn dummy_hash_is_valid_argon2() {
        assert!(DUMMY_HASH.as_ref().starts_with("$argon2"));
    }

    #[test]
    fn dummy_verify_does_not_panic() {
        dummy_verify();
    }

    #[test]
    fn wrong_secret_fails() {
        let claims = Claims::builder("user123", "users")
            .email("test@example.com")
            .exp((chrono::Utc::now().timestamp() as u64) + 3600)
            .build();
        let token = create_token(&claims, "correct-secret").unwrap();
        assert!(validate_token(&token, "wrong-secret").is_err());
    }

    #[test]
    fn verify_password_with_invalid_hash_returns_error() {
        // A corrupted or non-Argon2 string should return Err, not panic.
        let result = verify_password("password", "not-a-valid-hash");
        assert!(result.is_err());

        // Looks like a hash but is truncated/corrupted.
        let result = verify_password("password", "$argon2id$v=19$m=19456,t=2,p=1$AAAA$CORRUPT");
        assert!(result.is_err());
    }

    #[test]
    fn validate_token_with_malformed_jwt_returns_error() {
        // Completely non-JWT string.
        assert!(validate_token("not-a-jwt", "secret").is_err());

        // Wrong number of segments (JWT requires exactly 3 base64url parts).
        assert!(validate_token("aaa.bbb", "secret").is_err());

        // Four segments instead of three.
        assert!(validate_token("aaa.bbb.ccc.ddd", "secret").is_err());

        // Correct segment count but invalid base64 content.
        assert!(validate_token("!!!.???.$$$", "secret").is_err());
    }

    #[test]
    fn reset_token_error_display() {
        assert_eq!(ResetTokenError::NotFound.to_string(), "Invalid reset token");
        assert_eq!(
            ResetTokenError::Expired.to_string(),
            "Reset token has expired"
        );
    }

    #[test]
    fn reset_token_error_downcast_roundtrip() {
        let err: anyhow::Error = ResetTokenError::Expired.into();
        assert_eq!(
            err.downcast_ref::<ResetTokenError>(),
            Some(&ResetTokenError::Expired)
        );

        let err: anyhow::Error = ResetTokenError::NotFound.into();
        assert_eq!(
            err.downcast_ref::<ResetTokenError>(),
            Some(&ResetTokenError::NotFound)
        );
    }

    #[test]
    fn hash_password_empty_string_succeeds() {
        // An empty password is technically valid and should produce a usable hash.
        let hash = hash_password("").unwrap();
        assert!(hash.as_ref().starts_with("$argon2"));
        // And we can round-trip verify it.
        assert!(verify_password("", hash.as_ref()).unwrap());
        assert!(!verify_password("notempty", hash.as_ref()).unwrap());
    }

    #[test]
    fn create_and_validate_token_all_claims_match() {
        let exp = (chrono::Utc::now().timestamp() as u64) + 7200;
        let claims = Claims::builder("abc-123", "admins")
            .email("admin@example.com")
            .exp(exp)
            .build();
        let token = create_token(&claims, "roundtrip-secret").unwrap();
        let decoded = validate_token(&token, "roundtrip-secret").unwrap();
        assert_eq!(decoded.sub, claims.sub);
        assert_eq!(decoded.collection, claims.collection);
        assert_eq!(decoded.email, claims.email);
        assert_eq!(decoded.exp, claims.exp);
    }
}
