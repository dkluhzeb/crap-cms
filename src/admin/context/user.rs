//! Authenticated-user context — what templates see at `{{user.*}}`.

use schemars::JsonSchema;
use serde::Serialize;

use crate::{core::Claims, typegen::LuaAnnotation};

/// Identifying data about the currently authenticated user.
#[derive(Serialize, JsonSchema)]
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

impl LuaAnnotation for UserContext {
    fn render_lua_annotation(out: &mut String) {
        out.push_str("---@class crap.template.user\n");
        out.push_str("---@field id string\n");
        out.push_str("---@field email string\n");
        out.push_str("---@field collection string\n");
        out.push('\n');
    }
}
