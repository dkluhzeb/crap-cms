use anyhow::{Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::core::{DocumentId, Slug};

/// JWT claims for auth tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject — the document ID of the user.
    pub sub: DocumentId,
    /// Which auth collection this user belongs to.
    pub collection: Slug,
    /// User email.
    pub email: String,
    /// Expiration time (Unix timestamp).
    pub exp: u64,
    /// Issued-at time (Unix timestamp). Optional for backward compatibility with
    /// tokens issued before this field was added. Refreshed on every token
    /// reissue — do NOT use this to enforce a session absolute max age.
    #[serde(default)]
    pub iat: Option<u64>,
    /// Original authentication time (Unix timestamp). Set on initial login
    /// (password / MFA / OAuth / password-reset finalize) and **preserved
    /// across token refreshes**, so `auth.session_absolute_max_age` can be
    /// enforced as a hard ceiling on cumulative session lifetime. Optional
    /// for backward compatibility with tokens minted before this field
    /// existed — in that case the refresh handler falls back to `iat`.
    ///
    /// Name mirrors OIDC's `auth_time` claim for familiarity.
    #[serde(default)]
    pub auth_time: Option<u64>,
    /// Session version counter — incremented on password change. Tokens with an older
    /// version are rejected during validation.
    #[serde(default)]
    pub session_version: u64,
}

impl Claims {
    /// Start building a new `Claims` instance.
    pub fn builder(sub: impl Into<DocumentId>, collection: impl Into<Slug>) -> ClaimsBuilder {
        ClaimsBuilder::new(sub, collection)
    }
}

/// Builder for [`Claims`].
///
/// `sub` and `collection` are taken in `new()` (always required).
/// `email` and `exp` are set via chained methods.
pub struct ClaimsBuilder {
    sub: DocumentId,
    collection: Slug,
    email: Option<String>,
    exp: Option<u64>,
    auth_time: Option<u64>,
    session_version: u64,
}

impl ClaimsBuilder {
    /// Create a new `ClaimsBuilder` with the required `sub` and `collection` fields.
    pub fn new(sub: impl Into<DocumentId>, collection: impl Into<Slug>) -> Self {
        Self {
            sub: sub.into(),
            collection: collection.into(),
            email: None,
            exp: None,
            auth_time: None,
            session_version: 0,
        }
    }

    /// Set the user's email address.
    #[must_use]
    pub fn email(mut self, email: impl Into<String>) -> Self {
        self.email = Some(email.into());

        self
    }

    /// Set the expiration time (Unix timestamp).
    #[must_use]
    pub fn exp(mut self, exp: u64) -> Self {
        self.exp = Some(exp);

        self
    }

    /// Set the original authentication time (Unix timestamp).
    ///
    /// Login paths should pass `now()`; token refresh should forward the
    /// previous token's `auth_time` so the session's absolute max age is
    /// measured from the original login, not from each refresh.
    #[must_use]
    pub fn auth_time(mut self, at: u64) -> Self {
        self.auth_time = Some(at);

        self
    }

    /// Set the session version (incremented on password change).
    #[must_use]
    pub fn session_version(mut self, version: u64) -> Self {
        self.session_version = version;

        self
    }

    /// Build the final `Claims` instance.
    ///
    /// # Errors
    ///
    /// Returns an error if `email` or `exp` have not been set on the builder.
    pub fn build(self) -> Result<Claims> {
        let Some(email) = self.email else {
            bail!("ClaimsBuilder: email is required");
        };

        let Some(exp) = self.exp else {
            bail!("ClaimsBuilder: exp is required");
        };

        Ok(Claims {
            sub: self.sub,
            collection: self.collection,
            email,
            exp,
            iat: Some(Utc::now().timestamp().max(0).cast_unsigned()),
            auth_time: self.auth_time,
            session_version: self.session_version,
        })
    }
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
mod builder_tests {
    use super::*;

    #[test]
    fn builds_claims_with_all_fields() {
        let before = Utc::now().timestamp() as u64;
        let claims = ClaimsBuilder::new("user-id", "users")
            .email("user@example.com")
            .exp(9999999999)
            .build()
            .unwrap();
        let after = Utc::now().timestamp() as u64;
        assert_eq!(claims.sub, "user-id");
        assert_eq!(claims.collection, "users");
        assert_eq!(claims.email, "user@example.com");
        assert_eq!(claims.exp, 9999999999);
        assert_eq!(claims.session_version, 0, "default session_version is 0");
        let iat = claims.iat.expect("iat should be set");
        assert!(
            iat >= before && iat <= after,
            "iat should be current timestamp"
        );
    }

    #[test]
    fn session_version_set_via_builder() {
        let claims = ClaimsBuilder::new("user-id", "users")
            .email("user@example.com")
            .exp(9999999999)
            .session_version(42)
            .build()
            .unwrap();
        assert_eq!(claims.session_version, 42);
    }

    /// Regression: missing email must return an error, not panic.
    #[test]
    fn error_without_email() {
        let err = ClaimsBuilder::new("id", "col").exp(1).build().unwrap_err();
        assert!(
            err.to_string().contains("email is required"),
            "unexpected error: {err}"
        );
    }

    /// Regression: `build()` must produce a non-zero `iat` field that
    /// represents the current time (guards against `.max(0)` being absent
    /// or `iat` being left as `None`).
    #[test]
    fn build_produces_nonzero_iat() {
        let claims = ClaimsBuilder::new("u1", "users")
            .email("test@test.com")
            .exp(9999999999)
            .build()
            .unwrap();

        let iat = claims.iat.expect("iat must be Some");
        assert!(iat > 0, "iat must be non-zero, got {iat}");
        // Sanity: iat should be a reasonable Unix timestamp (after 2020-01-01)
        assert!(
            iat > 1_577_836_800,
            "iat should be after 2020-01-01, got {iat}"
        );
    }

    /// Regression: missing exp must return an error, not panic.
    #[test]
    fn error_without_exp() {
        let err = ClaimsBuilder::new("id", "col")
            .email("a@b.com")
            .build()
            .unwrap_err();
        assert!(
            err.to_string().contains("exp is required"),
            "unexpected error: {err}"
        );
    }

    // ── auth_time claim (audit finding M-4) ───────────────────────────────

    #[test]
    fn builds_claims_with_auth_time_set() {
        let claims = ClaimsBuilder::new("u", "c")
            .email("a@b.com")
            .exp(9999999999)
            .auth_time(1_700_000_000)
            .build()
            .unwrap();
        assert_eq!(claims.auth_time, Some(1_700_000_000));
    }

    #[test]
    fn builds_claims_leaves_auth_time_none_when_unset() {
        // Backwards compat: legacy login paths that haven't adopted
        // `auth_time` yet produce tokens without the claim. The refresh
        // handler must fall back to `iat` — not panic — in that case.
        let claims = ClaimsBuilder::new("u", "c")
            .email("a@b.com")
            .exp(9999999999)
            .build()
            .unwrap();
        assert!(claims.auth_time.is_none());
    }

    #[test]
    fn claims_auth_time_serializes_round_trip() {
        let claims = ClaimsBuilder::new("u", "c")
            .email("a@b.com")
            .exp(9999999999)
            .auth_time(1_700_000_000)
            .build()
            .unwrap();
        let json = serde_json::to_string(&claims).unwrap();
        assert!(json.contains("\"auth_time\":1700000000"));
        let round: Claims = serde_json::from_str(&json).unwrap();
        assert_eq!(round.auth_time, Some(1_700_000_000));
    }

    #[test]
    fn claims_missing_auth_time_deserializes_as_none() {
        // A legacy JWT minted before this claim existed must still decode.
        let json = r#"{
            "sub": "u",
            "collection": "users",
            "email": "a@b.com",
            "exp": 9999999999,
            "session_version": 0
        }"#;
        let claims: Claims = serde_json::from_str(json).unwrap();
        assert!(claims.auth_time.is_none());
    }
}
