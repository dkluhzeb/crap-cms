//! Authenticated-user context — what templates see at `{{user.*}}`.

use serde::Serialize;

use crate::core::auth::Claims;

/// Identifying data about the currently authenticated user.
#[derive(Serialize)]
pub struct UserContext {
    pub email: String,
    pub id: String,
    pub collection: String,
}

impl UserContext {
    /// Build from JWT claims.
    pub fn from_claims(claims: &Claims) -> Self {
        Self {
            email: claims.email.clone(),
            id: claims.sub.to_string(),
            collection: claims.collection.to_string(),
        }
    }
}
