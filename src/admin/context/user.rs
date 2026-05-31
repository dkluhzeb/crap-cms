//! Authenticated-user context — what templates see at `{{user.*}}`.

use schemars::JsonSchema;
use serde::Serialize;

use crate::{core::Claims, typegen::LuaAnnotation};

/// Identifying data about the currently authenticated user.
#[derive(Serialize, JsonSchema, LuaAnnotation)]
#[lua(class = "crap.template.user")]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_claims_maps_sub_email_and_collection() {
        let claims = Claims::builder("user-1", "admins")
            .email("a@example.com")
            .exp(9_999_999_999)
            .build()
            .unwrap();

        let ctx = UserContext::from_claims(&claims);
        assert_eq!(ctx.id, "user-1");
        assert_eq!(ctx.email, "a@example.com");
        assert_eq!(ctx.collection, "admins");
    }
}
