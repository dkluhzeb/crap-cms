use crate::core::auth::ClaimsBuilder;
use serde::{Deserialize, Serialize};

/// JWT claims for auth tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject — the document ID of the user.
    pub sub: String,
    /// Which auth collection this user belongs to.
    pub collection: String,
    /// User email.
    pub email: String,
    /// Expiration time (Unix timestamp).
    pub exp: u64,
}

impl Claims {
    /// Start building a new `Claims` instance.
    pub fn builder(sub: impl Into<String>, collection: impl Into<String>) -> ClaimsBuilder {
        ClaimsBuilder::new(sub, collection)
    }
}
