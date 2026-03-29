//! Builder for `crate::core::auth::Claims`.

use anyhow::{Result, bail};
use chrono::Utc;

use crate::core::{Claims, DocumentId, Slug};

/// Builder for [`Claims`].
///
/// `sub` and `collection` are taken in `new()` (always required).
/// `email` and `exp` are set via chained methods.
pub struct ClaimsBuilder {
    sub: DocumentId,
    collection: Slug,
    email: Option<String>,
    exp: Option<u64>,
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
            session_version: 0,
        }
    }

    /// Set the user's email address.
    pub fn email(mut self, email: impl Into<String>) -> Self {
        self.email = Some(email.into());
        self
    }

    /// Set the expiration time (Unix timestamp).
    pub fn exp(mut self, exp: u64) -> Self {
        self.exp = Some(exp);
        self
    }

    /// Set the session version (incremented on password change).
    pub fn session_version(mut self, version: u64) -> Self {
        self.session_version = version;
        self
    }

    /// Build the final `Claims` instance.
    ///
    /// Returns an error if `email` or `exp` have not been set.
    pub fn build(self) -> Result<Claims> {
        let email = match self.email {
            Some(e) => e,
            None => bail!("ClaimsBuilder: email is required"),
        };

        let exp = match self.exp {
            Some(e) => e,
            None => bail!("ClaimsBuilder: exp is required"),
        };

        Ok(Claims {
            sub: self.sub,
            collection: self.collection,
            email,
            exp,
            iat: Some(Utc::now().timestamp() as u64),
            session_version: self.session_version,
        })
    }
}

#[cfg(test)]
mod tests {
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
            "unexpected error: {}",
            err
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
            "unexpected error: {}",
            err
        );
    }
}
