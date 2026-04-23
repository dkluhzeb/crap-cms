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
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);

        jsonwebtoken::encode(&header, claims, &key).context("Failed to create JWT token")
    }

    fn validate_token(&self, token: &str) -> Result<Claims> {
        let key = jsonwebtoken::DecodingKey::from_secret(self.secret.as_bytes());

        // Pin the algorithm explicitly: `Validation::new(HS256)` refuses any
        // token whose header declares a different `alg`, closing the classic
        // "alg: none" / HS-vs-RS key-confusion class of attacks. Keep
        // `required_spec_claims` at its default (which includes `exp`) so a
        // token missing the expiration claim is rejected outright — previously
        // the field was cleared, which would have accepted tokens without an
        // `exp` if a caller ever produced one.
        let validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);

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

    // ── L-1: algorithm pinning + required exp claim ───────────────────────

    #[test]
    fn rejects_token_signed_with_different_algorithm() {
        // Mint a token using a different algorithm (HS512) but the same
        // secret. A permissive `Validation::default()` from older
        // jsonwebtoken versions could let this through; pinning to HS256
        // must reject it.
        let secret = "test-secret";
        let encoding_key = jsonwebtoken::EncodingKey::from_secret(secret.as_bytes());
        let hs512_header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS512);

        let claims = Claims::builder("u", "users")
            .email("a@b.com")
            .exp((chrono::Utc::now().timestamp() as u64) + 3600)
            .build()
            .unwrap();

        let hs512_token = jsonwebtoken::encode(&hs512_header, &claims, &encoding_key).unwrap();

        let err = JwtTokenProvider::new(secret)
            .validate_token(&hs512_token)
            .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("invalid")
                || err.to_string().to_lowercase().contains("alg"),
            "expected algorithm-mismatch error, got: {err}",
        );
    }

    #[test]
    fn rejects_token_missing_exp_claim() {
        // Hand-craft a JWT with no `exp` claim. The provider must reject it
        // via `required_spec_claims` — previously cleared, which would have
        // treated the missing claim as "skip the expiry check" instead of
        // "fail the token".
        #[derive(serde::Serialize)]
        struct NoExpClaims {
            sub: &'static str,
            collection: &'static str,
            email: &'static str,
        }

        let secret = "test-secret";
        let encoding_key = jsonwebtoken::EncodingKey::from_secret(secret.as_bytes());
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
        let no_exp = NoExpClaims {
            sub: "u",
            collection: "users",
            email: "a@b.com",
        };

        let token = jsonwebtoken::encode(&header, &no_exp, &encoding_key).unwrap();

        assert!(
            JwtTokenProvider::new(secret)
                .validate_token(&token)
                .is_err(),
            "token missing `exp` must be rejected",
        );
    }
}
