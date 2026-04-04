//! Token provider trait and JWT implementation.

use std::sync::Arc;

use anyhow::{Context as _, Result};

use crate::core::Claims;

/// Thread-safe shared reference to a token provider.
pub type SharedTokenProvider = Arc<dyn TokenProvider>;

/// Object-safe token provider trait.
///
/// Abstracts token creation and validation. The default implementation
/// uses JWT (jsonwebtoken crate). Rarely swapped — exists for testability
/// and potential future backends (opaque tokens, Paseto, etc.).
pub trait TokenProvider: Send + Sync {
    /// Create a signed token from claims.
    fn create_token(&self, claims: &Claims) -> Result<String>;

    /// Validate a token and return decoded claims.
    fn validate_token(&self, token: &str) -> Result<Claims>;

    /// Backend identifier.
    fn kind(&self) -> &'static str;
}

/// JWT token provider using the `jsonwebtoken` crate.
pub struct JwtTokenProvider {
    secret: String,
}

impl JwtTokenProvider {
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            secret: secret.into(),
        }
    }
}

impl TokenProvider for JwtTokenProvider {
    fn create_token(&self, claims: &Claims) -> Result<String> {
        let key = jsonwebtoken::EncodingKey::from_secret(self.secret.as_bytes());
        jsonwebtoken::encode(&jsonwebtoken::Header::default(), claims, &key)
            .context("Failed to create JWT token")
    }

    fn validate_token(&self, token: &str) -> Result<Claims> {
        let key = jsonwebtoken::DecodingKey::from_secret(self.secret.as_bytes());
        let mut validation = jsonwebtoken::Validation::default();

        validation.required_spec_claims.clear();
        validation.validate_exp = true;

        let data = jsonwebtoken::decode::<Claims>(token, &key, &validation)
            .context("Invalid JWT token")?;

        Ok(data.claims)
    }

    fn kind(&self) -> &'static str {
        "jwt"
    }
}

/// Create a signed JWT token from claims.
///
/// Free function for direct usage (admin middleware, upload auth).
/// For provider-based usage, prefer `TokenProvider::create_token`.
pub fn create_token(claims: &Claims, secret: &str) -> Result<String> {
    JwtTokenProvider::new(secret).create_token(claims)
}

/// Validate a JWT token and return the claims.
///
/// Free function for direct usage (admin middleware, upload auth).
/// For provider-based usage, prefer `TokenProvider::validate_token`.
pub fn validate_token(token: &str, secret: &str) -> Result<Claims> {
    JwtTokenProvider::new(secret).validate_token(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> JwtTokenProvider {
        JwtTokenProvider::new("test-secret")
    }

    #[test]
    fn token_roundtrip() {
        let p = provider();
        let claims = Claims::builder("user1", "users")
            .email("test@example.com")
            .exp((chrono::Utc::now().timestamp() as u64) + 3600)
            .build()
            .unwrap();

        let token = p.create_token(&claims).unwrap();
        let decoded = p.validate_token(&token).unwrap();
        assert_eq!(decoded.sub, "user1");
        assert_eq!(decoded.email, "test@example.com");
    }

    #[test]
    fn wrong_secret_fails() {
        let p1 = JwtTokenProvider::new("correct");
        let p2 = JwtTokenProvider::new("wrong");
        let claims = Claims::builder("u", "c")
            .email("e")
            .exp((chrono::Utc::now().timestamp() as u64) + 3600)
            .build()
            .unwrap();

        let token = p1.create_token(&claims).unwrap();
        assert!(p2.validate_token(&token).is_err());
    }

    #[test]
    fn expired_token_fails() {
        let p = provider();
        let claims = Claims::builder("u", "c").email("e").exp(0).build().unwrap();

        let token = p.create_token(&claims).unwrap();
        assert!(p.validate_token(&token).is_err());
    }

    #[test]
    fn kind_is_jwt() {
        assert_eq!(provider().kind(), "jwt");
    }
}
