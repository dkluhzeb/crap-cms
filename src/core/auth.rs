//! Authentication primitives: Argon2id password hashing and JWT token management.

use std::sync::LazyLock;

use anyhow::{Context, Result};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use serde::{Deserialize, Serialize};

/// Pre-computed Argon2 hash used to burn CPU time on user-not-found paths,
/// preventing timing oracles that leak whether an email exists.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    hash_password("__crap_dummy_timing__").expect("dummy hash")
});

/// Perform a dummy password verification to equalize timing with real verifications.
/// Call this on login paths where user-not-found or hash-missing would otherwise
/// return fast, enabling email enumeration via response timing.
pub fn dummy_verify() {
    let _ = verify_password("x", &DUMMY_HASH);
}

/// Hash a password using Argon2id.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("Password hashing failed: {}", e))?;
    Ok(hash.to_string())
}

/// Verify a password against a stored hash.
pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(hash)
        .map_err(|e| anyhow::anyhow!("Invalid password hash: {}", e))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// Full authenticated user context stored in request extensions.
/// Contains the JWT claims and the full user document from the database.
#[derive(Debug, Clone)]
pub struct AuthUser {
    #[allow(dead_code)]
    pub claims: Claims,
    pub user_doc: crate::core::Document,
}

/// JWT claims for auth tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject — the document ID of the user.
    pub sub: String,
    /// Which auth collection this user belongs to.
    pub collection: String,
    /// User email.
    pub email: String,
    /// Expiration time (Unix timestamp).
    pub exp: u64,
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
    let data = jsonwebtoken::decode::<Claims>(token, &key, &validation)
        .context("Invalid JWT token")?;
    Ok(data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify_password() {
        let hash = hash_password("secret123").unwrap();
        assert!(verify_password("secret123", &hash).unwrap());
        assert!(!verify_password("wrong", &hash).unwrap());
    }

    #[test]
    fn create_and_validate_token() {
        let claims = Claims {
            sub: "user123".to_string(),
            collection: "users".to_string(),
            email: "test@example.com".to_string(),
            exp: (chrono::Utc::now().timestamp() as u64) + 3600,
        };
        let token = create_token(&claims, "test-secret").unwrap();
        let decoded = validate_token(&token, "test-secret").unwrap();
        assert_eq!(decoded.sub, "user123");
        assert_eq!(decoded.email, "test@example.com");
    }

    #[test]
    fn expired_token_fails() {
        let claims = Claims {
            sub: "user123".to_string(),
            collection: "users".to_string(),
            email: "test@example.com".to_string(),
            exp: 0, // expired
        };
        let token = create_token(&claims, "test-secret").unwrap();
        assert!(validate_token(&token, "test-secret").is_err());
    }

    #[test]
    fn dummy_hash_is_valid_argon2() {
        assert!(DUMMY_HASH.starts_with("$argon2"));
    }

    #[test]
    fn dummy_verify_does_not_panic() {
        dummy_verify();
    }

    #[test]
    fn wrong_secret_fails() {
        let claims = Claims {
            sub: "user123".to_string(),
            collection: "users".to_string(),
            email: "test@example.com".to_string(),
            exp: (chrono::Utc::now().timestamp() as u64) + 3600,
        };
        let token = create_token(&claims, "correct-secret").unwrap();
        assert!(validate_token(&token, "wrong-secret").is_err());
    }
}
