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
