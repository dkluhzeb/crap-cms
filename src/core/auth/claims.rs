use crate::core::{DocumentId, Slug, auth::ClaimsBuilder};
use serde::{Deserialize, Serialize};

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
    /// tokens issued before this field was added.
    #[serde(default)]
    pub iat: Option<u64>,
}

impl Claims {
    /// Start building a new `Claims` instance.
    pub fn builder(sub: impl Into<DocumentId>, collection: impl Into<Slug>) -> ClaimsBuilder {
        ClaimsBuilder::new(sub, collection)
    }
}
