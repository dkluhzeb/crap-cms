//! Authenticated user context for request extensions.

use crate::core::{Claims, Document};

/// Full authenticated user context stored in request extensions.
/// Contains the JWT claims and the full user document from the database.
#[derive(Debug, Clone)]
pub struct AuthUser {
    /// The decoded JWT claims for this user.
    pub claims: Claims,
    /// The full document representing this user from their auth collection.
    pub user_doc: Document,
    /// Preferred admin UI locale (e.g. "en", "de"). Loaded from user settings.
    pub ui_locale: String,
}

impl AuthUser {
    /// Create a new `AuthUser` instance with the given claims and document.
    #[must_use]
    pub fn new(claims: Claims, user_doc: Document) -> Self {
        Self {
            claims,
            user_doc,
            ui_locale: "en".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_defaults_ui_locale_to_en() {
        let claims = Claims::builder("u1", "users")
            .email("u@example.com")
            .exp(9_999_999_999)
            .build()
            .unwrap();
        let user = AuthUser::new(claims, Document::new("u1"));
        assert_eq!(user.ui_locale, "en");
    }
}
