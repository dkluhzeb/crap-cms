//! Builder for `crate::core::auth::Claims`.

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
}

impl ClaimsBuilder {
    /// Create a new `ClaimsBuilder` with the required `sub` and `collection` fields.
    pub fn new(sub: impl Into<DocumentId>, collection: impl Into<Slug>) -> Self {
        Self {
            sub: sub.into(),
            collection: collection.into(),
            email: None,
            exp: None,
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

    /// Build the final `Claims` instance.
    ///
    /// # Panics
    ///
    /// Panics if `email` or `exp` have not been set.
    pub fn build(self) -> Claims {
        Claims {
            sub: self.sub,
            collection: self.collection,
            email: self.email.expect("ClaimsBuilder: email is required"),
            exp: self.exp.expect("ClaimsBuilder: exp is required"),
            iat: Some(Utc::now().timestamp() as u64),
        }
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
            .build();
        let after = Utc::now().timestamp() as u64;
        assert_eq!(claims.sub, "user-id");
        assert_eq!(claims.collection, "users");
        assert_eq!(claims.email, "user@example.com");
        assert_eq!(claims.exp, 9999999999);
        let iat = claims.iat.expect("iat should be set");
        assert!(
            iat >= before && iat <= after,
            "iat should be current timestamp"
        );
    }

    #[test]
    #[should_panic(expected = "ClaimsBuilder: email is required")]
    fn panics_without_email() {
        ClaimsBuilder::new("id", "col").exp(1).build();
    }

    #[test]
    #[should_panic(expected = "ClaimsBuilder: exp is required")]
    fn panics_without_exp() {
        ClaimsBuilder::new("id", "col").email("a@b.com").build();
    }
}
