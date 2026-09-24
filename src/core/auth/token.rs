//! Token provider trait and JWT implementation.

use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use chrono::Utc;

use crate::core::Claims;
use crate::core::auth::claims::TokenUse;

/// Thread-safe shared reference to a token provider.
pub type SharedTokenProvider = Arc<dyn TokenProvider>;

/// Object-safe token provider trait.
///
/// Abstracts token creation and validation. The default implementation
/// uses JWT (jsonwebtoken crate). Rarely swapped — exists for testability
/// and potential future backends (opaque tokens, Paseto, etc.).
pub trait TokenProvider: Send + Sync {
    /// Create a signed token from claims.
    ///
    /// Claims of a strategy-authenticated request ([`TokenUse::Strategy`])
    /// are never signed: a strategy's credential must not be exchangeable
    /// for a token.
    ///
    /// # Errors
    ///
    /// Returns an error if the claims are strategy claims, or if signing
    /// fails (e.g. claim serialization).
    fn create_token(&self, claims: &Claims) -> Result<String>;

    /// Validate a **session** token and return decoded claims.
    ///
    /// Rejects any token whose [`Claims::token_use`] is not
    /// [`TokenUse::Session`] — notably the short-lived MFA-pending token,
    /// which must never authenticate a request. Every authenticated surface
    /// (admin cookie/bearer, gRPC, upload serve) goes through here, so the
    /// discriminator check is enforced in one place rather than at each
    /// call site.
    ///
    /// # Errors
    ///
    /// Returns an error if the token is malformed, has an invalid signature,
    /// is expired, is missing required claims, or is not a session token.
    fn validate_token(&self, token: &str) -> Result<Claims>;

    /// Validate an **MFA-pending** token and return decoded claims.
    ///
    /// The mirror of [`validate_token`](Self::validate_token) for the one
    /// endpoint that legitimately consumes a pending token: it accepts only
    /// [`TokenUse::MfaPending`] and rejects a full session token.
    ///
    /// # Errors
    ///
    /// Returns an error if the token is malformed, has an invalid signature,
    /// is expired, is missing required claims, or is not an MFA-pending token.
    fn validate_pending_token(&self, token: &str) -> Result<Claims>;

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
        if claims.token_use == TokenUse::Strategy {
            bail!("strategy claims are never signed into a token");
        }

        let key = jsonwebtoken::EncodingKey::from_secret(self.secret.as_bytes());
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);

        jsonwebtoken::encode(&header, claims, &key).context("Failed to create JWT token")
    }

    fn validate_token(&self, token: &str) -> Result<Claims> {
        let claims = self.decode(token)?;

        if claims.token_use != TokenUse::Session {
            bail!("token is not a session token");
        }

        Ok(claims)
    }

    fn validate_pending_token(&self, token: &str) -> Result<Claims> {
        let claims = self.decode(token)?;

        if claims.token_use != TokenUse::MfaPending {
            bail!("token is not an MFA-pending token");
        }

        Ok(claims)
    }

    fn kind(&self) -> &'static str {
        "jwt"
    }
}

impl JwtTokenProvider {
    /// Decode + verify a JWT and return its claims, WITHOUT checking
    /// [`Claims::token_use`]. Private so the purpose check can't be skipped
    /// by an outside caller — public validation always goes through
    /// [`validate_token`](TokenProvider::validate_token) or
    /// [`validate_pending_token`](TokenProvider::validate_pending_token).
    fn decode(&self, token: &str) -> Result<Claims> {
        let key = jsonwebtoken::DecodingKey::from_secret(self.secret.as_bytes());

        // Pin the algorithm explicitly: `Validation::new(HS256)` refuses any
        // token whose header declares a different `alg`, closing the classic
        // "alg: none" / HS-vs-RS key-confusion class of attacks. Keep
        // `required_spec_claims` at its default (which includes `exp`) so a
        // token missing the expiration claim is rejected outright — previously
        // the field was cleared, which would have accepted tokens without an
        // `exp` if a caller ever produced one.
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
        // No grace period past `exp`: jsonwebtoken defaults to 60 seconds of
        // leeway, and even at zero it rejects only `exp < now`.
        validation.leeway = 0;

        let data = jsonwebtoken::decode::<Claims>(token, &key, &validation)
            .context("Invalid JWT token")?;

        // A token is dead AT its expiry second, matching every other expiry
        // check here (MFA codes, reset and verification tokens, signed URLs).
        let now = Utc::now().timestamp().max(0).cast_unsigned();
        if data.claims.exp <= now {
            bail!("Invalid JWT token: expired");
        }

        Ok(data.claims)
    }
}

/// Create a signed JWT token from claims.
///
/// Free function for direct usage (admin middleware, upload auth).
/// For provider-based usage, prefer `TokenProvider::create_token`.
///
/// # Errors
///
/// Returns an error if signing fails.
pub fn create_token(claims: &Claims, secret: &str) -> Result<String> {
    JwtTokenProvider::new(secret).create_token(claims)
}

/// Validate a JWT token and return the claims.
///
/// Free function for direct usage (admin middleware, upload auth).
/// For provider-based usage, prefer `TokenProvider::validate_token`.
///
/// # Errors
///
/// Returns an error if the token is malformed, has an invalid signature,
/// is expired, or is missing required claims.
pub fn validate_token(token: &str, secret: &str) -> Result<Claims> {
    JwtTokenProvider::new(secret).validate_token(token)
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use super::*;

    fn provider() -> JwtTokenProvider {
        JwtTokenProvider::new("test-secret")
    }

    /// A token is invalid the moment it expires — no library leeway.
    #[test]
    fn an_expired_token_is_rejected_without_leeway() {
        let p = provider();
        let claims = Claims::builder("user1", "users")
            .email("test@example.com")
            .exp((Utc::now().timestamp() as u64) - 5)
            .build()
            .unwrap();

        let token = p.create_token(&claims).unwrap();

        assert!(p.validate_token(&token).is_err());
    }

    /// A token whose `exp` is the current second is already expired.
    #[test]
    fn a_token_is_invalid_in_its_expiry_second() {
        let p = provider();
        let claims = Claims::builder("user1", "users")
            .email("test@example.com")
            .exp(Utc::now().timestamp() as u64)
            .build()
            .unwrap();

        let token = p.create_token(&claims).unwrap();

        assert!(p.validate_token(&token).is_err());
    }

    #[test]
    fn token_roundtrip() {
        let p = provider();
        let claims = Claims::builder("user1", "users")
            .email("test@example.com")
            .exp((Utc::now().timestamp() as u64) + 3600)
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
            .exp((Utc::now().timestamp() as u64) + 3600)
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

    // ── MFA bypass regression: token_use discriminator ────────────────────
    //
    // A short-lived MFA-pending token must NOT authenticate a session on any
    // surface. Before the `token_use` claim existed, the pending token was a
    // valid session JWT — an attacker who knew the password could copy the
    // `crap_mfa_pending` cookie value into `crap_session` and skip MFA.

    fn pending_claims() -> Claims {
        Claims::builder("u", "users")
            .email("a@b.com")
            .exp((Utc::now().timestamp() as u64) + 3600)
            .token_use(TokenUse::MfaPending)
            .build()
            .unwrap()
    }

    #[test]
    fn validate_token_rejects_mfa_pending_token() {
        let p = provider();
        let token = p.create_token(&pending_claims()).unwrap();

        let err = p.validate_token(&token).unwrap_err();
        assert!(
            err.to_string().contains("not a session token"),
            "MFA-pending token must not validate as a session, got: {err}",
        );
    }

    #[test]
    fn validate_pending_token_accepts_mfa_pending_token() {
        let p = provider();
        let token = p.create_token(&pending_claims()).unwrap();

        let claims = p.validate_pending_token(&token).unwrap();
        assert_eq!(claims.token_use, TokenUse::MfaPending);
    }

    #[test]
    fn validate_pending_token_rejects_session_token() {
        let p = provider();
        let session = Claims::builder("u", "users")
            .email("a@b.com")
            .exp((Utc::now().timestamp() as u64) + 3600)
            .build()
            .unwrap();
        let token = p.create_token(&session).unwrap();

        let err = p.validate_pending_token(&token).unwrap_err();
        assert!(
            err.to_string().contains("not an MFA-pending token"),
            "a full session token must not complete MFA, got: {err}",
        );
    }

    /// Claims built for a strategy-authenticated request must never become a
    /// signed token, whatever path hands them to the provider.
    #[test]
    fn create_token_refuses_strategy_claims() {
        let strategy = Claims::builder("u", "users")
            .email("a@b.com")
            .exp((Utc::now().timestamp() as u64) + 3600)
            .token_use(TokenUse::Strategy)
            .build()
            .unwrap();

        let err = provider().create_token(&strategy).unwrap_err();

        assert!(err.to_string().contains("never signed"), "got: {err}");
    }

    #[test]
    fn validate_token_accepts_session_token() {
        let p = provider();
        let session = Claims::builder("u", "users")
            .email("a@b.com")
            .exp((Utc::now().timestamp() as u64) + 3600)
            .build()
            .unwrap();
        let token = p.create_token(&session).unwrap();

        assert_eq!(
            p.validate_token(&token).unwrap().token_use,
            TokenUse::Session,
        );
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
            .exp((Utc::now().timestamp() as u64) + 3600)
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
