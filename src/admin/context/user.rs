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
